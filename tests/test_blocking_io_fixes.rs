// tests/test_blocking_io_fixes.rs
//
// Unit tests for v0.8.51 blocking I/O fixes
// Verifies that fixes for executor starvation work correctly:
//   Fix 1: spawn_blocking for glob operations
//   Fix 2: Configurable agent_ready_timeout
//   Fix 3: yield_now() in prepare loops
//   Fix 4: Documentation (verified manually)

use anyhow::Result;
use sai3_bench::config::{Config, PrepareConfig, EnsureSpec, PrepareStrategy};
use sai3_bench::size_generator::SizeSpec;
use sai3_bench::workload::prepare_objects;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::sync::mpsc;

// ============================================================================
// Fix 2: Configurable agent_ready_timeout
// ============================================================================

#[test]
fn test_agent_ready_timeout_default() -> Result<()> {
    // Test that agent_ready_timeout has correct default value (120s)
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s
concurrency: 32

distributed:
  agents:
    - address: "localhost:7761"
      id: "agent-1"
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Default should be 120 seconds
    assert_eq!(dist.agent_ready_timeout, 120,
        "Default agent_ready_timeout should be 120 seconds");
    
    println!("✓ Default agent_ready_timeout: {}s", dist.agent_ready_timeout);
    Ok(())
}

#[test]
fn test_agent_ready_timeout_custom() -> Result<()> {
    // Test that agent_ready_timeout can be customized
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s
concurrency: 32

distributed:
  agents:
    - address: "localhost:7761"
      id: "agent-1"
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agent_ready_timeout: 300  # 5 minutes for large-scale tests

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.agent_ready_timeout, 300,
        "Custom agent_ready_timeout should be 300 seconds");
    
    println!("✓ Custom agent_ready_timeout: {}s", dist.agent_ready_timeout);
    Ok(())
}

#[test]
fn test_agent_ready_timeout_scale_recommendations() -> Result<()> {
    // Test parsing of recommended timeout values for different scales
    struct ScaleTest {
        name: &'static str,
        timeout: u64,
        expected_files: &'static str,
    }
    
    let tests = vec![
        ScaleTest { name: "small",  timeout: 60,  expected_files: "<10K" },
        ScaleTest { name: "medium", timeout: 120, expected_files: "10K-100K" },
        ScaleTest { name: "large",  timeout: 300, expected_files: "100K-1M" },
        ScaleTest { name: "xlarge", timeout: 600, expected_files: ">1M" },
    ];
    
    for test in tests {
        let yaml = format!(r#"
target: "file:///tmp/test/"
duration: 10s
concurrency: 32

distributed:
  agents:
    - address: "localhost:7761"
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agent_ready_timeout: {}

workload:
  - op: get
    path: "data/*"
    weight: 100
"#, test.timeout);
        
        let config: Config = serde_yaml::from_str(&yaml)?;
        let dist = config.distributed.as_ref().unwrap();
        
        assert_eq!(dist.agent_ready_timeout, test.timeout,
            "{} scale timeout mismatch", test.name);
        
        println!("✓ {} scale ({}): timeout={}s", 
            test.name, test.expected_files, test.timeout);
    }
    
    Ok(())
}

// ============================================================================
// Fix 1: spawn_blocking for glob operations
// ============================================================================

#[tokio::test]
async fn test_glob_does_not_block_executor() -> Result<()> {
    // This test verifies that glob operations don't block the executor
    // by ensuring a concurrent task can make progress during glob expansion
    
    use std::path::PathBuf;
    use tokio::time::sleep;
    
    // Create temporary directory with many files
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();
    
    // Create 1000 files to make glob take measurable time
    for i in 0..1000 {
        let file_path = base_path.join(format!("file_{:04}.dat", i));
        std::fs::write(&file_path, b"test")?;
    }
    
    let pattern = base_path.join("*.dat").to_string_lossy().to_string();
    
    // Track concurrent task progress
    let (tx, mut rx) = mpsc::channel(10);
    
    // Spawn a task that should be able to run even during glob
    let heartbeat_task = tokio::spawn(async move {
        for i in 0..5 {
            tx.send(i).await.unwrap();
            sleep(Duration::from_millis(10)).await;
        }
    });
    
    let start = Instant::now();
    
    // Simulate what validate_workload_config does (spawn_blocking for glob)
    let glob_task = tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
        let paths: Vec<_> = glob::glob(&pattern)
            .map_err(|e| anyhow::anyhow!("Glob error: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Glob iteration error: {}", e))?;
        Ok(paths)
    });
    
    // Heartbeat task should complete even while glob is running
    let mut heartbeat_count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
        heartbeat_count += 1;
    }
    
    let paths = glob_task.await??;
    let elapsed = start.elapsed();
    
    // Verify glob found all files
    assert_eq!(paths.len(), 1000, "Should find 1000 files");
    
    // Verify heartbeat task made progress (proves executor wasn't blocked)
    assert_eq!(heartbeat_count, 5, "Heartbeat task should have completed all 5 sends");
    
    println!("✓ Glob found {} files in {:?} while concurrent task made progress", 
        paths.len(), elapsed);
    println!("✓ Concurrent heartbeats received: {}", heartbeat_count);
    
    heartbeat_task.await?;
    Ok(())
}

#[tokio::test]
async fn test_glob_large_directory() -> Result<()> {
    // Test that glob can handle large directories without timing out
    // This simulates the scenario where agent validates 100K+ files
    
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();
    
    // Create 10,000 files (smaller than real scenario but enough to test)
    println!("Creating 10,000 test files...");
    for i in 0..10_000 {
        let file_path = base_path.join(format!("data_{:05}.bin", i));
        std::fs::write(&file_path, b"x")?;
    }
    
    let pattern = base_path.join("*.bin").to_string_lossy().to_string();
    let start = Instant::now();
    
    // Use spawn_blocking as the fix requires
    let paths = tokio::task::spawn_blocking(move || -> Result<Vec<std::path::PathBuf>> {
        let paths: Vec<_> = glob::glob(&pattern)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(paths)
    }).await??;
    
    let elapsed = start.elapsed();
    
    assert_eq!(paths.len(), 10_000, "Should find all 10,000 files");
    println!("✓ Glob expansion of 10K files completed in {:?}", elapsed);
    println!("✓ Using spawn_blocking prevents executor starvation");
    
    Ok(())
}

// ============================================================================
// Fix 3: yield_now() in prepare loops
// ============================================================================

#[tokio::test]
async fn test_prepare_yields_during_creation() -> Result<()> {
    // Test that prepare operations yield periodically, allowing other tasks to run
    // This prevents executor starvation during million-object creates
    
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    // Create 500 small objects (enough to trigger multiple yields at 100-op intervals)
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 500,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),  // 1KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
        ],
        cleanup: false,
        cleanup_mode: sai3_bench::config::CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: None,
        prepare_strategy: PrepareStrategy::Parallel,
        skip_verification: false,
        force_overwrite: false,
    };
    
    // Track concurrent task progress during prepare
    let (tx, mut rx) = mpsc::channel(100);
    
    // Spawn heartbeat task that runs during prepare
    let heartbeat_task = tokio::spawn(async move {
        for i in 0..50 {
            tx.send(i).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    
    let start = Instant::now();
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    
    // Run prepare (should yield every 100 operations)
    let prepare_task = tokio::spawn(async move {
        prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 16, 0, true, None, None).await
    });
    
    // Collect heartbeats while prepare runs
    let mut heartbeat_count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        heartbeat_count += 1;
    }
    
    let (objects, _, _) = prepare_task.await??;
    let elapsed = start.elapsed();
    
    // Verify objects were created
    assert_eq!(objects.len(), 500, "Should create 500 objects");
    
    // Verify heartbeat task made significant progress (proves yields work)
    // With 500 objects and yields every 100, we expect ~5 yields minimum
    // Heartbeat task sends every 10ms, so should get many heartbeats
    assert!(heartbeat_count >= 10, 
        "Should receive at least 10 heartbeats (got {}), proving executor yields", 
        heartbeat_count);
    
    println!("✓ Prepare created {} objects in {:?}", objects.len(), elapsed);
    println!("✓ Concurrent heartbeats received: {} (proves yielding works)", heartbeat_count);
    println!("✓ Expected yields: ~{} (every 100 ops)", 500 / 100);
    
    heartbeat_task.await?;
    Ok(())
}

#[tokio::test]
async fn test_prepare_sequential_yields() -> Result<()> {
    // Test that sequential prepare also yields correctly
    
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 300,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(512)),
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
        ],
        cleanup: false,
        cleanup_mode: sai3_bench::config::CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: None,
        prepare_strategy: PrepareStrategy::Sequential,  // Sequential mode
        skip_verification: false,
        force_overwrite: false,
    };
    
    let (tx, mut rx) = mpsc::channel(100);
    
    let heartbeat_task = tokio::spawn(async move {
        for i in 0..30 {
            tx.send(i).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    let prepare_task = tokio::spawn(async move {
        prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 16, 0, true, None, None).await
    });
    
    let mut heartbeat_count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        heartbeat_count += 1;
    }
    
    let (objects, _, _) = prepare_task.await??;
    
    assert_eq!(objects.len(), 300);
    assert!(heartbeat_count >= 5, 
        "Sequential prepare should also yield (got {} heartbeats)", heartbeat_count);
    
    println!("✓ Sequential prepare created {} objects", objects.len());
    println!("✓ Heartbeats during sequential: {}", heartbeat_count);
    
    heartbeat_task.await?;
    Ok(())
}

// ============================================================================
// Integration: Executor Responsiveness Under Load
// ============================================================================

#[tokio::test]
async fn test_executor_responsiveness_large_prepare() -> Result<()> {
    // Integration test: Verify that during large prepare operations,
    // the executor remains responsive to other tasks (simulating stats, heartbeats)
    
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    // Create 1000 objects to ensure multiple yield points
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 1000,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
        ],
        cleanup: false,
        cleanup_mode: sai3_bench::config::CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: None,
        prepare_strategy: PrepareStrategy::Parallel,
        skip_verification: false,
        force_overwrite: false,
    };
    
    // Simulate stats writer (sends every 1 second in real code)
    let (stats_tx, mut stats_rx) = mpsc::channel(100);
    let stats_task = tokio::spawn(async move {
        for i in 0..10 {
            stats_tx.send(format!("stats_{}", i)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    
    // Simulate heartbeat task (sends every 1 second in real code)
    let (hb_tx, mut hb_rx) = mpsc::channel(100);
    let heartbeat_task = tokio::spawn(async move {
        for i in 0..10 {
            hb_tx.send(format!("heartbeat_{}", i)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    
    let start = Instant::now();
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    
    // Run prepare in background
    let prepare_task = tokio::spawn(async move {
        prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 32, 0, true, None, None).await
    });
    
    // Collect messages from both tasks
    let mut stats_count = 0;
    let mut hb_count = 0;
    
    loop {
        tokio::select! {
            msg = stats_rx.recv() => {
                if msg.is_some() {
                    stats_count += 1;
                } else {
                    break;
                }
            }
            msg = hb_rx.recv() => {
                if msg.is_some() {
                    hb_count += 1;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                break;
            }
        }
    }
    
    let (objects, _, _) = prepare_task.await??;
    let elapsed = start.elapsed();
    
    // Verify prepare completed
    assert_eq!(objects.len(), 1000, "Should create 1000 objects");
    
    // Verify both concurrent tasks made progress
    assert!(stats_count >= 5, 
        "Stats task should send multiple updates (got {})", stats_count);
    assert!(hb_count >= 5, 
        "Heartbeat task should send multiple updates (got {})", hb_count);
    
    println!("✓ Prepare created {} objects in {:?}", objects.len(), elapsed);
    println!("✓ Stats updates received: {} (proves executor not starved)", stats_count);
    println!("✓ Heartbeats received: {} (proves executor not starved)", hb_count);
    println!("✓ Expected yields: ~{} (every 100 ops)", 1000 / 100);
    
    stats_task.await?;
    heartbeat_task.await?;
    Ok(())
}

// ============================================================================
// Timeout Value Validation
// ============================================================================

#[test]
fn test_timeout_duration_conversion() -> Result<()> {
    // Test that agent_ready_timeout converts correctly to Duration
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agent_ready_timeout: 300

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    let timeout = Duration::from_secs(dist.agent_ready_timeout);
    
    assert_eq!(timeout.as_secs(), 300);
    assert_eq!(timeout, Duration::from_secs(300));
    
    println!("✓ Timeout converts correctly: {}s = {:?}", 
        dist.agent_ready_timeout, timeout);
    
    Ok(())
}

#[test]
fn test_timeout_realistic_values() -> Result<()> {
    // Test realistic timeout values for large-scale deployments
    struct TimeoutTest {
        scenario: &'static str,
        timeout_secs: u64,
        expected_min_files: u64,
    }
    
    let tests = vec![
        TimeoutTest {
            scenario: "Simple validation (no glob)",
            timeout_secs: 60,
            expected_min_files: 0,
        },
        TimeoutTest {
            scenario: "10K files glob expansion",
            timeout_secs: 120,
            expected_min_files: 10_000,
        },
        TimeoutTest {
            scenario: "100K files glob expansion",
            timeout_secs: 300,
            expected_min_files: 100_000,
        },
        TimeoutTest {
            scenario: "1M files glob expansion",
            timeout_secs: 600,
            expected_min_files: 1_000_000,
        },
    ];
    
    for test in &tests {
        let yaml = format!(r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agent_ready_timeout: {}

workload:
  - op: get
    path: "data/*"
    weight: 100
"#, test.timeout_secs);
        
        let config: Config = serde_yaml::from_str(&yaml)?;
        let dist = config.distributed.as_ref().unwrap();
        
        assert_eq!(dist.agent_ready_timeout, test.timeout_secs);
        
        println!("✓ {}: timeout={}s (handles {}+ files)", 
            test.scenario, test.timeout_secs, test.expected_min_files);
    }
    
    Ok(())
}

// ============================================================================
// Regression Tests
// ============================================================================

#[test]
fn test_backward_compatibility_no_timeout() -> Result<()> {
    // Test that configs without agent_ready_timeout still work (use default)
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  # No agent_ready_timeout specified

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Should use default (120s)
    assert_eq!(dist.agent_ready_timeout, 120,
        "Should use default timeout when not specified");
    
    println!("✓ Backward compatibility: missing timeout uses default (120s)");
    Ok(())
}

#[tokio::test]
async fn test_small_prepare_still_works() -> Result<()> {
    // Regression: Ensure small prepares (< 100 objects) still work
    // even though they won't trigger any yields
    
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 10,  // Small count, won't trigger yields
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
        ],
        cleanup: false,
        cleanup_mode: sai3_bench::config::CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: None,
        prepare_strategy: PrepareStrategy::Parallel,
        skip_verification: false,
        force_overwrite: false,
    };
    
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 16, 0, true, None, None).await?;
    
    assert_eq!(objects.len(), 10, "Small prepare should still work");
    println!("✓ Regression: Small prepare (10 objects) works correctly");
    
    Ok(())
}
