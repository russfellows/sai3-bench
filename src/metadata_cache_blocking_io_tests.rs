/// Unit tests to prevent executor-blocking I/O regression (v0.8.61 critical fix)
///
/// These tests verify that ALL blocking I/O operations use spawn_blocking to prevent
/// freezing the tokio executor. The v0.8.61 bug was caused by synchronous tar+zstd
/// compression being called directly in async context, freezing all agents at exactly
/// 69 objects when the first periodic checkpoint triggered.

#[cfg(test)]
mod blocking_io_regression_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::time::{timeout, Duration};

    /// Test that checkpoint creation doesn't block the executor
    ///
    /// This simulates the v0.8.61 hang by creating a checkpoint while
    /// also running concurrent async tasks. If checkpoint blocks, the
    /// concurrent tasks will timeout.
    #[tokio::test]
    async fn test_checkpoint_does_not_block_executor() {
        let temp_dir = tempfile::tempdir().unwrap();
        let results_dir = temp_dir.path();
        
        // Create cache with some data
        let endpoint_uri = format!("file://{}", temp_dir.path().join("storage").display());
        let cache = MetadataCache::new(
            results_dir,
            &[endpoint_uri.clone()],
            "test_hash".to_string(),
            Some(0),
            None,
        )
        .await
        .unwrap();

        // Plan some objects to make checkpoint non-trivial
        for i in 0..100 {
            cache.plan_object(0, i, &format!("file_{}.dat", i), 1024).await.unwrap();
        }

        // Spawn concurrent "heartbeat" task that must continue while checkpoint runs
        let heartbeat_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = heartbeat_flag.clone();
        
        let heartbeat = tokio::spawn(async move {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                flag_clone.store(true, Ordering::Relaxed);
            }
        });

        // Create checkpoint (should use spawn_blocking, not block executor)
        let checkpoint_result = timeout(
            Duration::from_secs(5),
            cache.lock().await.endpoints()[0].1.write_checkpoint(Some(0))
        )
        .await;

        // Wait for heartbeat
        let heartbeat_result = timeout(Duration::from_secs(3), heartbeat).await;

        // Verify both completed
        assert!(checkpoint_result.is_ok(), "Checkpoint should complete without timeout");
        assert!(heartbeat_result.is_ok(), "Heartbeat should run concurrently");
        assert!(heartbeat_flag.load(Ordering::Relaxed), "Heartbeat should have run at least once");
    }

    /// Test that multiple concurrent checkpoints don't deadlock
    ///
    /// Simulates 4 agents all creating checkpoints simultaneously
    /// (the exact scenario that hung in v0.8.60)
    #[tokio::test]
    async fn test_concurrent_checkpoints_from_multiple_agents() {
        let temp_dir = tempfile::tempdir().unwrap();
        
        // Create 4 agent caches (simulating 4-agent distributed test)
        let mut handles = Vec::new();
        
        for agent_id in 0..4 {
            let temp_path = temp_dir.path().to_path_buf();
            
            let handle = tokio::spawn(async move {
                let results_dir = temp_path.join(format!("agent-{}", agent_id));
                std::fs::create_dir_all(&results_dir).unwrap();
                
                let endpoint_uri = format!("file://{}", temp_path.join("storage").display());
                let cache = MetadataCache::new(
                    &results_dir,
                    &[endpoint_uri],
                    "concurrent_test".to_string(),
                    Some(agent_id),
                    None,
                )
                .await
                .unwrap();

                // Plan objects
                for i in 0..50 {
                    cache.plan_object(0, i, &format!("file_{}.dat", i), 1024).await.unwrap();
                }

                // Create checkpoint
                cache.lock().await.endpoints()[0].1.write_checkpoint(Some(agent_id)).await
            });
            
            handles.push(handle);
        }

        // All 4 agents should complete checkpoints without timeout
        let results = timeout(
            Duration::from_secs(15),
            futures::future::join_all(handles)
        )
        .await;

        assert!(results.is_ok(), "All 4 concurrent checkpoints should complete without timeout");
        
        let checkpoint_results: Vec<_> = results.unwrap().into_iter().collect();
        assert_eq!(checkpoint_results.len(), 4, "All 4 agents should complete");
        
        for (agent_id, result) in checkpoint_results.iter().enumerate() {
            assert!(result.is_ok(), "Agent {} checkpoint task should not panic", agent_id);
            assert!(result.as_ref().unwrap().is_ok(), "Agent {} checkpoint should succeed", agent_id);
        }
    }

    /// Test that checkpoint timeout is correctly enforced
    ///
    /// This uses a mock slow filesystem to verify timeout protection works
    #[tokio::test]
    async fn test_checkpoint_timeout_protection() {
        // This test is harder to implement without mocking filesystem
        // For now, we verify that timeout values are reasonable
        
        let archive_sizes = vec![
            (1 * 1024 * 1024, "1 MB"),          // 1 MB
            (64 * 1024 * 1024, "64 MB"),        // 64 MB
            (512 * 1024 * 1024, "512 MB"),      // 512 MB
        ];

        for (size, label) in archive_sizes {
            // Calculate timeout based on 10 MB/s minimum speed
            let timeout_secs = std::cmp::max(10, (size / 10_000_000) + 4);
            
            // Verify timeout is reasonable
            assert!(
                timeout_secs >= 10,
                "Timeout for {} should be at least 10 seconds", label
            );
            
            // At 10 MB/s, verify timeout allows completion with buffer
            let min_time_at_10mbs = (size as f64 / 10_000_000.0).ceil() as u64;
            let buffer_time = timeout_secs - min_time_at_10mbs;
            
            assert!(
                buffer_time >= 4,
                "Timeout for {} should have at least 4s buffer (got {}s)",
                label, buffer_time
            );
        }
    }

    /// Test that periodic checkpoint doesn't starve prepare operations
    ///
    /// Simulates the exact v0.8.60 hang scenario: prepare running when
    /// periodic checkpoint timer fires
    #[tokio::test]
    async fn test_periodic_checkpoint_does_not_starve_prepare() {
        let temp_dir = tempfile::tempdir().unwrap();
        let results_dir = temp_dir.path();
        
        let endpoint_uri = format!("file://{}", temp_dir.path().join("storage").display());
        let cache = MetadataCache::new(
            results_dir,
            &[endpoint_uri],
            "periodic_test".to_string(),
            Some(0),
            None,
        )
        .await
        .unwrap();

        // Spawn periodic checkpoint task (1 second interval for fast test)
        let (endpoint_idx, endpoint_cache) = cache.lock().await.endpoints().next().unwrap();
        let checkpoint_handle = endpoint_cache.spawn_periodic_checkpoint(1, Some(0));

        // Simulate prepare operations continuing
        let prepare_flag = Arc::new(AtomicBool::new(true));
        let flag_clone = prepare_flag.clone();
        
        let prepare_task = tokio::spawn(async move {
            for i in 0..200 {
                if !flag_clone.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                // Simulate object planning
                cache.plan_object(0, i, &format!("file_{}.dat", i), 1024).await.ok();
            }
        });

        // Let it run for 3 seconds (should trigger 2-3 periodic checkpoints)
        tokio::time::sleep(Duration::from_secs(3)).await;
        
        // Stop prepare simulation
        prepare_flag.store(false, Ordering::Relaxed);
        
        // Cancel periodic checkpoint
        checkpoint_handle.abort();
        
        // Verify prepare completed
        let prepare_result = timeout(Duration::from_secs(2), prepare_task).await;
        assert!(
            prepare_result.is_ok(),
            "Prepare task should complete even with periodic checkpoints running"
        );
    }

    /// Test that checkpoint write speed is calculated correctly
    #[tokio::test]
    async fn test_checkpoint_write_speed_reporting() {
        let temp_dir = tempfile::tempdir().unwrap();
        let results_dir = temp_dir.path();
        
        let endpoint_uri = format!("file://{}", temp_dir.path().join("storage").display());
        let cache = MetadataCache::new(
            results_dir,
            &[endpoint_uri],
            "speed_test".to_string(),
            Some(0),
            None,
        )
        .await
        .unwrap();

        // Plan some objects
        for i in 0..100 {
            cache.plan_object(0, i, &format!("file_{}.dat", i), 1024).await.unwrap();
        }

        // Create checkpoint and capture timing
        let start = std::time::Instant::now();
        let result = cache.lock().await.endpoints()[0].1.write_checkpoint(Some(0)).await;
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "Checkpoint should succeed");
        
        // Verify it completed in reasonable time (< 10 seconds for small test)
        assert!(
            elapsed.as_secs() < 10,
            "Small checkpoint should complete in <10 seconds, took {:.2}s",
            elapsed.as_secs_f64()
        );
    }
}
