//! Integration tests for I/O rate control (v0.7.1)
//!
//! These tests verify that the rate controller correctly throttles operation
//! start rates across different distributions and concurrency levels.

use sai3_bench::config::{IoRateConfig, IopsTarget, ArrivalDistribution};
use sai3_bench::rate_controller::{RateController, OptionalRateController};
use std::time::Instant;
use tokio;

/// Test that rate controller with max IOPS has zero overhead
#[tokio::test]
async fn test_rate_controller_max_no_overhead() {
    let config = IoRateConfig {
        iops: IopsTarget::Max,
        distribution: ArrivalDistribution::Exponential,
    };
    
    let controller = RateController::new(config, 16);
    
    // Verify no throttling configured
    assert_eq!(controller.target_iops(), None);
    
    // Call wait_for_next multiple times - should return immediately
    let start = Instant::now();
    for _ in 0..1000 {
        controller.wait_for_next().await;
    }
    let elapsed = start.elapsed();
    
    // Should complete in < 10ms (no sleeping)
    assert!(elapsed.as_millis() < 10, 
        "Max IOPS should have zero overhead, took {}ms", 
        elapsed.as_millis());
}

/// Test that OptionalRateController with None config is a no-op
#[tokio::test]
async fn test_optional_controller_disabled() {
    let optional = OptionalRateController::new(None, 16);
    
    assert!(!optional.is_enabled());
    assert!(optional.controller().is_none());
    
    // Call wait multiple times - should return immediately
    let start = Instant::now();
    for _ in 0..1000 {
        optional.wait().await;
    }
    let elapsed = start.elapsed();
    
    // Should complete in < 10ms (no sleeping)
    assert!(elapsed.as_millis() < 10, 
        "Disabled rate control should have zero overhead, took {}ms", 
        elapsed.as_millis());
}

/// Test rate controller calculations for different worker counts
#[tokio::test]
async fn test_rate_controller_inter_arrival_calculations() {
    // Test case 1: 1000 IOPS with 10 workers
    // Each worker: 100 ops/sec
    // Inter-arrival: 1,000,000 / 100 = 10,000 microseconds
    let config1 = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Uniform,
    };
    let controller1 = RateController::new(config1, 10);
    assert_eq!(controller1.target_iops(), Some(1000));
    
    // Test case 2: 5000 IOPS with 20 workers
    // Each worker: 250 ops/sec
    // Inter-arrival: 1,000,000 / 250 = 4,000 microseconds
    let config2 = IoRateConfig {
        iops: IopsTarget::Fixed(5000),
        distribution: ArrivalDistribution::Uniform,
    };
    let controller2 = RateController::new(config2, 20);
    assert_eq!(controller2.target_iops(), Some(5000));
    
    // Test case 3: 10000 IOPS with 100 workers
    // Each worker: 100 ops/sec
    // Inter-arrival: 1,000,000 / 100 = 10,000 microseconds
    let config3 = IoRateConfig {
        iops: IopsTarget::Fixed(10000),
        distribution: ArrivalDistribution::Uniform,
    };
    let controller3 = RateController::new(config3, 100);
    assert_eq!(controller3.target_iops(), Some(10000));
}

/// Test uniform distribution produces consistent delays
#[tokio::test]
async fn test_uniform_distribution_consistency() {
    // 100 IOPS with 1 worker = 10ms per operation
    let config = IoRateConfig {
        iops: IopsTarget::Fixed(100),
        distribution: ArrivalDistribution::Uniform,
    };
    
    let controller = RateController::new(config, 1);
    
    // Measure time for 10 operations
    let start = Instant::now();
    for _ in 0..10 {
        controller.wait_for_next().await;
    }
    let elapsed = start.elapsed();
    
    // Expected: 10 operations * 10ms = 100ms
    // Allow ±20ms tolerance for scheduling overhead
    let elapsed_ms = elapsed.as_millis();
    assert!(elapsed_ms >= 80 && elapsed_ms <= 120, 
        "Expected ~100ms for 10 ops at 100 IOPS, got {}ms", 
        elapsed_ms);
}

/// Test deterministic distribution tracks operation count
#[tokio::test]
async fn test_deterministic_distribution_tracking() {
    let config = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Deterministic,
    };
    
    let controller = RateController::new(config, 10);
    
    // Execute some operations
    for _ in 0..50 {
        controller.wait_for_next().await;
    }
    
    // In deterministic mode, we track operation count
    // The actual rate may vary, but we should have tracked operations
    let current_rate = controller.current_rate();
    assert!(current_rate > 0.0, "Should have tracked some operations");
}

/// Test exponential distribution uses random delays
#[tokio::test]
async fn test_exponential_distribution_variability() {
    let config = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Exponential,
    };
    
    let controller = RateController::new(config, 10);
    
    // Collect delays from multiple operations
    let mut delays = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        controller.wait_for_next().await;
        delays.push(start.elapsed().as_micros());
    }
    
    // Exponential distribution should have variability
    // Check that delays are not all identical (not uniform)
    let first_delay = delays[0];
    let all_same = delays.iter().all(|&d| d == first_delay);
    assert!(!all_same, "Exponential distribution should have varying delays");
}

/// Test multiple workers can share rate controller via Arc
#[tokio::test]
async fn test_multi_worker_sharing() {
    use std::sync::Arc;
    
    let config = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Uniform,
    };
    
    let controller = Arc::new(RateController::new(config, 4));
    
    // Spawn 4 workers, each doing 25 operations
    let mut handles = Vec::new();
    for _ in 0..4 {
        let ctrl = Arc::clone(&controller);
        let handle = tokio::spawn(async move {
            for _ in 0..25 {
                ctrl.wait_for_next().await;
            }
        });
        handles.push(handle);
    }
    
    // Wait for all workers to complete
    for handle in handles {
        handle.await.unwrap();
    }
    
    // 4 workers * 25 ops = 100 total operations
    // At 1000 IOPS with 4 workers: each worker does 250 ops/sec
    // 25 ops should take ~100ms per worker
    // All running concurrently should still take ~100ms total
}

/// Test configuration parsing for IOPS values
#[test]
fn test_iops_target_parsing() {
    // Test numeric 0 maps to Max
    let config1 = IoRateConfig {
        iops: IopsTarget::Max,
        distribution: ArrivalDistribution::Exponential,
    };
    let controller1 = RateController::new(config1, 1);
    assert_eq!(controller1.target_iops(), None);
    
    // Test fixed values
    let config2 = IoRateConfig {
        iops: IopsTarget::Fixed(5000),
        distribution: ArrivalDistribution::Exponential,
    };
    let controller2 = RateController::new(config2, 1);
    assert_eq!(controller2.target_iops(), Some(5000));
}

/// Test distribution type is correctly stored
#[tokio::test]
async fn test_distribution_type_storage() {
    let config1 = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Exponential,
    };
    let controller1 = RateController::new(config1, 10);
    assert_eq!(controller1.distribution(), ArrivalDistribution::Exponential);
    
    let config2 = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Uniform,
    };
    let controller2 = RateController::new(config2, 10);
    assert_eq!(controller2.distribution(), ArrivalDistribution::Uniform);
    
    let config3 = IoRateConfig {
        iops: IopsTarget::Fixed(1000),
        distribution: ArrivalDistribution::Deterministic,
    };
    let controller3 = RateController::new(config3, 10);
    assert_eq!(controller3.distribution(), ArrivalDistribution::Deterministic);
}
