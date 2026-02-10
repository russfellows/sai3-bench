// Integration tests for distributed metadata cache
// Tests end-to-end workflows, multi-endpoint coordination, resume logic, and large-scale operations

use sai3_bench::metadata_cache::{MetadataCache, ObjectState};
use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// Helper Functions
// ============================================================================

fn setup_multi_endpoint_env(num_endpoints: usize) -> (TempDir, Vec<String>, PathBuf) {
    let temp = TempDir::new().unwrap();
    let results_dir = temp.path().join("results");
    std::fs::create_dir_all(&results_dir).unwrap();

    let endpoints: Vec<String> = (0..num_endpoints)
        .map(|i| format!("file://{}/endpoint_{}", temp.path().display(), i))
        .collect();

    (temp, endpoints, results_dir)
}

// ============================================================================
// Test Suite 1: Multi-Endpoint Coordination
// ============================================================================

#[tokio::test]
async fn test_4_endpoint_coordination() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(4);
    let config_hash = "4ep_test".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();
    assert_eq!(cache.num_endpoints(), 4);

    // Plan 100 objects, verify round-robin distribution
    for i in 0..100 {
        let ep_idx = i % 4;
        cache.endpoint(ep_idx).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 1024).unwrap();
    }

    // Verify each endpoint owns exactly 25 objects
    for ep_idx in 0..4 {
        let planned = cache.endpoint(ep_idx).unwrap()
            .get_objects_by_state(&config_hash, ObjectState::Planned).unwrap();
        assert_eq!(planned.len(), 25, "Endpoint {} should have 25 planned objects", ep_idx);
    }
}

#[tokio::test]
async fn test_multi_endpoint_progress_aggregation() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(3);
    let config_hash = "progress_3ep".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Endpoint 0: 30 created
    for i in (0..90).step_by(3) {
        cache.endpoint(0).unwrap().plan_object(&config_hash, i, &format!("ep0_file_{}.dat", i), 2048).unwrap();
        cache.endpoint(0).unwrap().mark_creating(&config_hash, i).unwrap();
        cache.endpoint(0).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
    }

    // Endpoint 1: 20 created, 10 failed
    for i in (1..91).step_by(3) {
        cache.endpoint(1).unwrap().plan_object(&config_hash, i, &format!("ep1_file_{}.dat", i), 2048).unwrap();
        if i < 61 {
            cache.endpoint(1).unwrap().mark_creating(&config_hash, i).unwrap();
            cache.endpoint(1).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
        } else {
            cache.endpoint(1).unwrap().mark_creating(&config_hash, i).unwrap();
            cache.endpoint(1).unwrap().mark_failed(&config_hash, i).unwrap();
        }
    }

    // Endpoint 2: 25 planned (not created yet)
    for i in (2..77).step_by(3) {
        cache.endpoint(2).unwrap().plan_object(&config_hash, i, &format!("ep2_file_{}.dat", i), 2048).unwrap();
    }

    // Aggregate progress
    let totals = cache.aggregate_progress(&config_hash).unwrap();
    assert_eq!(totals.get(&ObjectState::Created), Some(&50), "Should have 30 + 20 created");
    assert_eq!(totals.get(&ObjectState::Failed), Some(&10), "Should have 10 failed");
    assert_eq!(totals.get(&ObjectState::Planned), Some(&25), "Should have 25 planned");
}

// ============================================================================
// Test Suite 2: Resume Logic
// ============================================================================

#[tokio::test]
async fn test_resume_after_partial_prepare() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(2);
    let config_hash = "resume_test".to_string();

    // Phase 1: Initial prepare (simulate crash after 50 objects)
    {
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

        // Plan 100 objects
        for i in 0..100 {
            let ep_idx = i % 2;
            cache.endpoint(ep_idx).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 4096).unwrap();
        }

        // Create only first 50 objects (simulate crash)
        for i in 0..50 {
            let ep_idx = i % 2;
            cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, i).unwrap();
            cache.endpoint(ep_idx).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
        }

        cache.flush_all().unwrap();
    }

    // Phase 2: Resume (reopen cache, find Planned objects, complete them)
    {
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

        // Find objects that need creation
        let mut total_needs_creation = 0;
        for ep_idx in 0..2 {
            let planned = cache.endpoint(ep_idx).unwrap()
                .get_objects_by_state(&config_hash, ObjectState::Planned).unwrap();
            total_needs_creation += planned.len();

            // Complete the planned objects
            for (file_idx, _entry) in planned {
                cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, file_idx).unwrap();
                cache.endpoint(ep_idx).unwrap().mark_created(&config_hash, file_idx, Some(1738886500), None).unwrap();
            }
        }

        assert_eq!(total_needs_creation, 50, "Should have 50 objects needing creation");

        // Verify all objects now created
        let totals = cache.aggregate_progress(&config_hash).unwrap();
        assert_eq!(totals.get(&ObjectState::Created), Some(&100));
        assert!(
            totals.get(&ObjectState::Planned).is_none() || totals.get(&ObjectState::Planned) == Some(&0),
            "No objects should be in Planned state"
        );
    }
}

#[tokio::test]
async fn test_resume_after_failures() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(2);
    let config_hash = "resume_fail".to_string();

    // Phase 1: First attempt with failures
    {
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

        // Plan 50 objects, 30 succeed, 20 fail
        for i in 0..50 {
            let ep_idx = i % 2;
            cache.endpoint(ep_idx).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 8192).unwrap();

            if i < 30 {
                cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, i).unwrap();
                cache.endpoint(ep_idx).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
            } else {
                cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, i).unwrap();
                cache.endpoint(ep_idx).unwrap().mark_failed(&config_hash, i).unwrap();
            }
        }

        cache.flush_all().unwrap();
    }

    // Phase 2: Retry failed objects
    {
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

        // Find failed objects and retry
        let mut total_retries = 0;
        for ep_idx in 0..2 {
            let failed = cache.endpoint(ep_idx).unwrap()
                .get_objects_by_state(&config_hash, ObjectState::Failed).unwrap();
            total_retries += failed.len();

            // Retry (transition Failed → Creating → Created)
            for (file_idx, _entry) in failed {
                cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, file_idx).unwrap();
                cache.endpoint(ep_idx).unwrap().mark_created(&config_hash, file_idx, Some(1738886600), None).unwrap();
            }
        }

        assert_eq!(total_retries, 20, "Should retry 20 failed objects");

        // Verify all objects now created
        let totals = cache.aggregate_progress(&config_hash).unwrap();
        assert_eq!(totals.get(&ObjectState::Created), Some(&50));
        assert!(
            totals.get(&ObjectState::Failed).is_none() || totals.get(&ObjectState::Failed) == Some(&0),
            "No failed objects should remain"
        );
    }
}

// ============================================================================
// Test Suite 3: Drift Detection
// ============================================================================

#[tokio::test]
async fn test_drift_detection_external_deletion() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(1);
    let config_hash = "drift_detect".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Create 20 objects
    for i in 0..20 {
        cache.endpoint(0).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 1024).unwrap();
        cache.endpoint(0).unwrap().mark_creating(&config_hash, i).unwrap();
        cache.endpoint(0).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
    }

    // Verify all created
    let counts = cache.endpoint(0).unwrap().count_by_state(&config_hash).unwrap();
    assert_eq!(counts.get(&ObjectState::Created), Some(&20));

    // Simulate external deletion of 5 objects (mark as Deleted)
    for i in 10..15 {
        cache.endpoint(0).unwrap().mark_deleted(&config_hash, i).unwrap();
    }

    // Verify drift detected
    let counts = cache.endpoint(0).unwrap().count_by_state(&config_hash).unwrap();
    assert_eq!(counts.get(&ObjectState::Created), Some(&15), "Should have 15 created (20 - 5 deleted)");
    assert_eq!(counts.get(&ObjectState::Deleted), Some(&5), "Should detect 5 deleted objects");

    // Get list of missing objects
    let deleted = cache.endpoint(0).unwrap()
        .get_objects_by_state(&config_hash, ObjectState::Deleted).unwrap();
    assert_eq!(deleted.len(), 5);
    
    let deleted_indices: Vec<usize> = deleted.iter().map(|(idx, _)| *idx).collect();
    assert_eq!(deleted_indices, vec![10, 11, 12, 13, 14]);
}

#[tokio::test]
async fn test_pre_workload_validation() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(2);
    let config_hash = "validation".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Plan 100 objects, create 95, fail 5
    for i in 0..100 {
        let ep_idx = i % 2;
        cache.endpoint(ep_idx).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 2048).unwrap();

        if i < 95 {
            cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, i).unwrap();
            cache.endpoint(ep_idx).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
        } else {
            cache.endpoint(ep_idx).unwrap().mark_creating(&config_hash, i).unwrap();
            cache.endpoint(ep_idx).unwrap().mark_failed(&config_hash, i).unwrap();
        }
    }

    // Pre-workload validation: check all objects exist
    let totals = cache.aggregate_progress(&config_hash).unwrap();
    let created = totals.get(&ObjectState::Created).copied().unwrap_or(0);
    let failed = totals.get(&ObjectState::Failed).copied().unwrap_or(0);
    let deleted = totals.get(&ObjectState::Deleted).copied().unwrap_or(0);

    assert_eq!(created, 95);
    assert_eq!(failed, 5);
    assert_eq!(deleted, 0);

    // Validation result: 5 objects missing (failed)
    let validation_ok = failed == 0 && deleted == 0;
    assert!(!validation_ok, "Validation should fail due to 5 failed objects");
}

// ============================================================================
// Test Suite 4: Config Hash Invalidation
// ============================================================================

#[tokio::test]
async fn test_config_hash_change_invalidation() {
    let temp = TempDir::new().unwrap();
    let results_dir = temp.path().join("results");
    std::fs::create_dir_all(&results_dir).unwrap();
    let endpoints = vec![format!("file://{}/ep0", temp.path().display())];

    // Config 1: 100 files
    let config_hash_1 = sai3_bench::metadata_cache::compute_config_hash(100, None, &endpoints);
    
    // Phase 1: Create cache with config1, add objects, flush and drop
    {
        let cache_1 = MetadataCache::new(&results_dir, &endpoints, config_hash, None_1.clone()).await.unwrap();

        // Plan 100 objects
        for i in 0..100 {
            cache_1.endpoint(0).unwrap().plan_object(&config_hash_1, i, &format!("file_{}.dat", i), 1024).unwrap();
        }

        let counts_1 = cache_1.endpoint(0).unwrap().count_by_state(&config_hash_1).unwrap();
        assert_eq!(counts_1.get(&ObjectState::Planned), Some(&100));
        
        cache_1.flush_all().unwrap();
        // cache_1 dropped here
    }

    // Config 2: 200 files (different config hash)
    let config_hash_2 = sai3_bench::metadata_cache::compute_config_hash(200, None, &endpoints);
    assert_ne!(config_hash_1, config_hash_2, "Different configs should have different hashes");

    // Phase 2: Create new cache, verify config isolation
    {
        let cache_2 = MetadataCache::new(&results_dir, &endpoints, config_hash, None_2.clone()).await.unwrap();

        // Old config data should NOT be visible with new config hash
        let counts_2 = cache_2.endpoint(0).unwrap().count_by_state(&config_hash_2).unwrap();
        assert_eq!(counts_2.get(&ObjectState::Planned), None, "Old config data should be isolated");

        // Can still access old config data with old hash
        let counts_1_again = cache_2.endpoint(0).unwrap().count_by_state(&config_hash_1).unwrap();
        assert_eq!(counts_1_again.get(&ObjectState::Planned), Some(&100), "Old config data should persist");
    }
}

// ============================================================================
// Test Suite 5: Listing Cache
// ============================================================================

#[tokio::test]
async fn test_listing_cache_compression() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(1);
    let config_hash = "listing_test".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Create 1000 object paths
    let objects: Vec<String> = (0..1000)
        .map(|i| format!("dir_{}/subdir_{}/file_{:04}.dat", i / 100, i / 10, i))
        .collect();

    // Put listing
    cache.coordinator_cache().put_listing(&config_hash, &objects).unwrap();

    // Get listing
    let retrieved = cache.coordinator_cache().get_listing(&config_hash).unwrap();
    assert!(retrieved.is_some(), "Listing should be cached");
    assert_eq!(retrieved.unwrap().len(), 1000, "Should retrieve all 1000 objects");
}

// ============================================================================
// Test Suite 6: Stress Tests
// ============================================================================

#[tokio::test]
async fn test_stress_100k_objects_single_endpoint() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(1);
    let config_hash = "stress_100k".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Plan 100,000 objects in batches
    let batch_size = 10_000;
    for batch_start in (0..100_000).step_by(batch_size) {
        let objects: Vec<(usize, String, u64)> = (batch_start..batch_start + batch_size)
            .map(|i| (i, format!("stress/file_{:06}.dat", i), 8192))
            .collect();

        cache.endpoint(0).unwrap().plan_objects_batch(&config_hash, &objects).unwrap();
    }

    // Verify count
    let counts = cache.endpoint(0).unwrap().count_by_state(&config_hash).unwrap();
    assert_eq!(counts.get(&ObjectState::Planned), Some(&100_000));

    // Mark 50,000 as created
    let start = std::time::Instant::now();
    for i in 0..50_000 {
        cache.endpoint(0).unwrap().mark_creating(&config_hash, i).unwrap();
        cache.endpoint(0).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
    }
    let duration = start.elapsed();
    println!("Created 50,000 objects in {:?}", duration);

    // Verify progress
    let counts = cache.endpoint(0).unwrap().count_by_state(&config_hash).unwrap();
    assert_eq!(counts.get(&ObjectState::Created), Some(&50_000));
    assert_eq!(counts.get(&ObjectState::Planned), Some(&50_000));
}

#[tokio::test]
async fn test_stress_10k_objects_per_endpoint_4_endpoints() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(4);
    let config_hash = "stress_4ep".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Each endpoint gets 10,000 objects (40,000 total)
    for ep_idx in 0..4 {
        let objects: Vec<(usize, String, u64)> = (0..10_000)
            .map(|i| {
                let file_idx = ep_idx * 10_000 + i;
                (file_idx, format!("ep{}/file_{:05}.dat", ep_idx, i), 4096)
            })
            .collect();

        cache.endpoint(ep_idx).unwrap().plan_objects_batch(&config_hash, &objects).unwrap();
    }

    // Verify distribution
    for ep_idx in 0..4 {
        let counts = cache.endpoint(ep_idx).unwrap().count_by_state(&config_hash).unwrap();
        assert_eq!(counts.get(&ObjectState::Planned), Some(&10_000), "Endpoint {} should have 10,000 objects", ep_idx);
    }

    // Aggregate
    let totals = cache.aggregate_progress(&config_hash).unwrap();
    assert_eq!(totals.get(&ObjectState::Planned), Some(&40_000));
}

// ============================================================================
// Test Suite 7: Persistence and Crash Recovery
// ============================================================================

#[tokio::test]
async fn test_persistence_across_cache_reopens() {
    let temp = TempDir::new().unwrap();
    let results_dir = temp.path().join("results");
    std::fs::create_dir_all(&results_dir).unwrap();

    let endpoints = vec![format!("file://{}/ep0", temp.path().display())];
    let config_hash = "persist_test".to_string();

    // Phase 1: Create cache, add 1000 objects, flush
    {
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

        for i in 0..1000 {
            cache.endpoint(0).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 2048).unwrap();
        }

        for i in 0..500 {
            cache.endpoint(0).unwrap().mark_creating(&config_hash, i).unwrap();
            cache.endpoint(0).unwrap().mark_created(&config_hash, i, Some(1738886400), None).unwrap();
        }

        cache.flush_all().unwrap();
        // Cache dropped here
    }

    // Phase 2: Reopen cache, verify data persisted
    {
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

        let counts = cache.endpoint(0).unwrap().count_by_state(&config_hash).unwrap();
        assert_eq!(counts.get(&ObjectState::Created), Some(&500), "Created objects should persist");
        assert_eq!(counts.get(&ObjectState::Planned), Some(&500), "Planned objects should persist");

        // Verify specific object
        let entry = cache.endpoint(0).unwrap().get_object(&config_hash, 250).unwrap().unwrap();
        assert_eq!(entry.state, ObjectState::Created);
        assert_eq!(entry.path, "file_250.dat");
        assert_eq!(entry.created_at, Some(1738886400));
    }
}

// ============================================================================
// Test Suite 8: Edge Cases
// ============================================================================

#[tokio::test]
async fn test_empty_endpoint() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(2);
    let config_hash = "empty_ep".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Endpoint 0: 100 objects
    for i in (0..200).step_by(2) {
        cache.endpoint(0).unwrap().plan_object(&config_hash, i, &format!("file_{}.dat", i), 1024).unwrap();
    }

    // Endpoint 1: 0 objects (empty)

    let totals = cache.aggregate_progress(&config_hash).unwrap();
    assert_eq!(totals.get(&ObjectState::Planned), Some(&100));

    // Verify endpoint 1 is empty
    let counts_ep1 = cache.endpoint(1).unwrap().count_by_state(&config_hash).unwrap();
    assert!(counts_ep1.is_empty(), "Endpoint 1 should have no objects");
}

#[tokio::test]
async fn test_missing_object_lookup() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(1);
    let config_hash = "missing".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Plan object 0
    cache.endpoint(0).unwrap().plan_object(&config_hash, 0, "file_0.dat", 1024).unwrap();

    // Lookup object 0 (exists)
    let obj = cache.endpoint(0).unwrap().get_object(&config_hash, 0).unwrap();
    assert!(obj.is_some());

    // Lookup object 999 (missing)
    let missing = cache.endpoint(0).unwrap().get_object(&config_hash, 999).unwrap();
    assert!(missing.is_none(), "Missing object should return None");
}

#[tokio::test]
async fn test_zero_size_objects() {
    let (_temp, endpoints, results_dir) = setup_multi_endpoint_env(1);
    let config_hash = "zero_size".to_string();

    let cache = MetadataCache::new(&results_dir, &endpoints, config_hash, None.clone()).await.unwrap();

    // Plan 10 zero-size objects
    for i in 0..10 {
        cache.endpoint(0).unwrap().plan_object(&config_hash, i, &format!("empty_{}.dat", i), 0).unwrap();
    }

    let counts = cache.endpoint(0).unwrap().count_by_state(&config_hash).unwrap();
    assert_eq!(counts.get(&ObjectState::Planned), Some(&10));

    // Verify retrieval
    let obj = cache.endpoint(0).unwrap().get_object(&config_hash, 5).unwrap().unwrap();
    assert_eq!(obj.size, 0);
    assert_eq!(obj.path, "empty_5.dat");
}
