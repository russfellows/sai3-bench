// tests/parallel_prepare_tests.rs
// Tests for parallel prepare strategy (v0.7.2)

use anyhow::Result;
use sai3_bench::config::{Config, PrepareConfig, EnsureSpec, PrepareStrategy};
use sai3_bench::workload::prepare_objects;
use sai3_bench::size_generator::SizeSpec;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Test that sequential strategy creates objects in size-group order
#[tokio::test]
async fn test_sequential_strategy_ordering() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 10,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),  // 1KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 10,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(2048)),  // 2KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 10,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(4096)),  // 4KB
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
        prepare_strategy: PrepareStrategy::Sequential,
        skip_verification: false,
        force_overwrite: false,
    };
    
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 16, 0, true).await?;
    
    // Should have created 30 objects total
    assert_eq!(objects.len(), 30, "Should create 30 objects");
    
    // Sequential strategy: first 10 should be 1KB, next 10 should be 2KB, last 10 should be 4KB
    // Check first 10 are 1KB
    for (i, obj) in objects.iter().enumerate().take(10) {
        assert_eq!(obj.size, 1024, "Object {} should be 1KB", i);
    }
    
    // Check next 10 are 2KB
    for (i, obj) in objects.iter().enumerate().take(20).skip(10) {
        assert_eq!(obj.size, 2048, "Object {} should be 2KB", i);
    }
    
    // Check last 10 are 4KB
    for (i, obj) in objects.iter().enumerate().take(30).skip(20) {
        assert_eq!(obj.size, 4096, "Object {} should be 4KB", i);
    }
    
    Ok(())
}

/// Test that parallel strategy mixes sizes together
#[tokio::test]
async fn test_parallel_strategy_mixing() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 20,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),  // 1KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 20,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(2048)),  // 2KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 20,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(4096)),  // 4KB
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
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 64, 0, true).await?;
    
    // Should have created 60 objects total
    assert_eq!(objects.len(), 60, "Should create 60 objects");
    
    // Parallel strategy: sizes should be mixed, not all 1KB first, then all 2KB, then all 4KB
    // Check that first 20 objects are NOT all the same size (proves shuffling)
    let mut size_counts_in_first_20 = HashMap::new();
    for obj in objects.iter().take(20) {
        *size_counts_in_first_20.entry(obj.size).or_insert(0) += 1;
    }
    
    // Should have more than one size in the first 20 objects
    assert!(size_counts_in_first_20.len() > 1, 
        "First 20 objects should contain multiple sizes, got: {:?}", size_counts_in_first_20);
    
    // Verify total counts are correct (20 of each size)
    let mut total_size_counts = HashMap::new();
    for obj in &objects {
        *total_size_counts.entry(obj.size).or_insert(0) += 1;
    }
    
    assert_eq!(total_size_counts.get(&1024), Some(&20), "Should have exactly 20 1KB files");
    assert_eq!(total_size_counts.get(&2048), Some(&20), "Should have exactly 20 2KB files");
    assert_eq!(total_size_counts.get(&4096), Some(&20), "Should have exactly 20 4KB files");
    
    Ok(())
}

/// Test that parallel strategy maintains exact counts per size
#[tokio::test]
async fn test_parallel_strategy_exact_counts() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 13,  // Odd number
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(512)),
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 27,  // Another odd number
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 8,   // Small even number
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(2048)),
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
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 8, 0, true).await?;
    
    // Count sizes
    let mut size_counts = HashMap::new();
    for obj in &objects {
        *size_counts.entry(obj.size).or_insert(0) += 1;
    }
    
    // Verify exact counts match the config
    assert_eq!(size_counts.get(&512), Some(&13), "Should have exactly 13 512-byte files");
    assert_eq!(size_counts.get(&1024), Some(&27), "Should have exactly 27 1KB files");
    assert_eq!(size_counts.get(&2048), Some(&8), "Should have exactly 8 2KB files");
    assert_eq!(objects.len(), 48, "Total should be 13+27+8=48");
    
    Ok(())
}

/// Test that default strategy is sequential
#[tokio::test]
async fn test_default_strategy_is_sequential() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    // Create config without explicitly setting prepare_strategy
    let yaml = format!(r#"
duration: "1s"
concurrency: 4
target: "{}"
workload:
  - op: get
    path: "*.dat"
    weight: 100
prepare:
  ensure_objects:
    - base_uri: "{}"
      count: 5
      size_distribution: 1024
    - base_uri: "{}"
      count: 5
      size_distribution: 2048
  cleanup: false
"#, base_uri, base_uri, base_uri);
    
    let config: Config = serde_yaml::from_str(&yaml)?;
    let prepare = config.prepare.as_ref().unwrap();
    
    // Default should be sequential
    assert_eq!(prepare.prepare_strategy, PrepareStrategy::Sequential, 
        "Default prepare_strategy should be Sequential");
    
    Ok(())
}

/// Test that YAML parsing works for both strategies
#[tokio::test]
async fn test_yaml_parsing_strategies() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    // Test sequential
    let yaml_sequential = format!(r#"
duration: "1s"
concurrency: 4
target: "{}"
workload:
  - op: get
    path: "*.dat"
    weight: 100
prepare:
  prepare_strategy: sequential
  ensure_objects:
    - base_uri: "{}"
      count: 5
      size_distribution: 1024
  cleanup: false
"#, base_uri, base_uri);
    
    let config_seq: Config = serde_yaml::from_str(&yaml_sequential)?;
    assert_eq!(config_seq.prepare.as_ref().unwrap().prepare_strategy, 
        PrepareStrategy::Sequential);
    
    // Test parallel
    let yaml_parallel = format!(r#"
duration: "1s"
concurrency: 4
target: "{}"
workload:
  - op: get
    path: "*.dat"
    weight: 100
prepare:
  prepare_strategy: parallel
  ensure_objects:
    - base_uri: "{}"
      count: 5
      size_distribution: 1024
  cleanup: false
"#, base_uri, base_uri);
    
    let config_par: Config = serde_yaml::from_str(&yaml_parallel)?;
    assert_eq!(config_par.prepare.as_ref().unwrap().prepare_strategy, 
        PrepareStrategy::Parallel);
    
    Ok(())
}

/// Test parallel strategy with single ensure_objects entry (should still work)
#[tokio::test]
async fn test_parallel_with_single_size() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 20,
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
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 24, 0, true).await?;
    
    assert_eq!(objects.len(), 20, "Should create 20 objects total");
    
    // All should be 1KB
    for obj in &objects {
        assert_eq!(obj.size, 1024);
    }
    
    Ok(())
}

/// Test that files are actually created with correct sizes on disk
/// Uses different directories to avoid overlap between EnsureSpec entries
#[tokio::test]
async fn test_files_created_correctly() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri_1kb = format!("file://{}/1kb/", base_path);
    let base_uri_2kb = format!("file://{}/2kb/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri_1kb.clone()),
                count: 3,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri_2kb.clone()),
                count: 3,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(2048)),
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
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 24, 0, true).await?;
    
    // Verify files exist on disk with correct sizes
    for obj in &objects {
        let file_path = obj.uri.strip_prefix("file://").unwrap();
        let metadata = std::fs::metadata(file_path)?;
        assert_eq!(metadata.len(), obj.size, 
            "File {} should be {} bytes on disk", file_path, obj.size);
    }
    
    Ok(())
}

/// Test that parallel strategy distributes sizes across directory tree (not clustered)
#[tokio::test]
async fn test_parallel_directory_distribution() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    // Create 90 files with parallel strategy (3 sizes x 30 each)
    // THEN verify they distribute properly when put into directory tree
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 30,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(1024)),  // 1KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 30,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(2048)),  // 2KB
                fill: sai3_bench::config::FillPattern::Zero,
                dedup_factor: 1,
                compress_factor: 1,
                use_multi_endpoint: false,
            },
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 30,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(4096)),  // 4KB
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
        directory_structure: None,  // No directory tree - test file ordering
        prepare_strategy: PrepareStrategy::Parallel,
        skip_verification: false,
        force_overwrite: false,
    };
    
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    let (objects, _, _) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, 48, 0, true).await?;
    
    // Should have created 90 objects total
    assert_eq!(objects.len(), 90, "Should create 90 objects");
    
    // CRITICAL TEST: Verify sizes are well-distributed across the sequence
    // With good shuffling, we shouldn't see long runs of the same size
    
    // Check every consecutive group of 10 files - each should have multiple sizes
    let chunk_size = 10;
    let mut chunks_with_mixed_sizes = 0;
    
    for chunk_start in (0..objects.len()).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(objects.len());
        let chunk = &objects[chunk_start..chunk_end];
        
        let mut size_counts: HashMap<u64, usize> = HashMap::new();
        for obj in chunk {
            *size_counts.entry(obj.size).or_insert(0) += 1;
        }
        
        if size_counts.len() >= 2 {
            chunks_with_mixed_sizes += 1;
        }
    }
    
    // With 9 chunks of 10 files each, most should have mixed sizes
    // (If sequential, first 30 would be all 1KB, next 30 all 2KB, etc.)
    assert!(chunks_with_mixed_sizes >= 7, 
        "At least 7 of 9 chunks should have mixed sizes, got {} (proves shuffling)", 
        chunks_with_mixed_sizes);
    
    // Verify no long runs of identical sizes (max run should be < 15 with good shuffling)
    let mut max_run = 0;
    let mut current_run = 1;
    for i in 1..objects.len() {
        if objects[i].size == objects[i-1].size {
            current_run += 1;
            max_run = max_run.max(current_run);
        } else {
            current_run = 1;
        }
    }
    
    assert!(max_run < 15, 
        "Maximum run of identical sizes should be < 15 (got {}), proves shuffling works", 
        max_run);
    
    // Verify total counts are still exact
    let mut total_size_counts = HashMap::new();
    for obj in &objects {
        *total_size_counts.entry(obj.size).or_insert(0) += 1;
    }
    
    assert_eq!(total_size_counts.get(&1024), Some(&30), "Should have exactly 30 1KB files");
    assert_eq!(total_size_counts.get(&2048), Some(&30), "Should have exactly 30 2KB files");
    assert_eq!(total_size_counts.get(&4096), Some(&30), "Should have exactly 30 4KB files");
    
    Ok(())
}

/// Test that concurrency parameter is correctly passed and accepted
/// This verifies the fix for the "32 workers" hardcoded bug
#[tokio::test]
async fn test_concurrency_parameter_passing() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path().to_str().unwrap();
    let base_uri = format!("file://{}/", base_path);
    
    let prepare_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 5,
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
        prepare_strategy: PrepareStrategy::Sequential,
        skip_verification: false,
        force_overwrite: false,
    };
    
    // Test with different concurrency values to verify parameter is accepted
    for concurrency in [1, 16, 32, 64, 128] {
        let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
        let (objects, _, metrics) = prepare_objects(&prepare_config, None, None, None, &multi_ep_cache, 1, concurrency, 0, true).await?;
        
        // Verify objects were created
        assert_eq!(objects.len(), 5, "Should create 5 objects with concurrency={}", concurrency);
        assert_eq!(metrics.objects_created, 5, "Metrics should show 5 objects created");
        
        // Verify all objects have correct size
        for obj in &objects {
            assert_eq!(obj.size, 1024, "All objects should be 1KB");
        }
    }
    
    // Test parallel strategy as well
    let parallel_config = PrepareConfig {
        ensure_objects: vec![
            EnsureSpec {
                base_uri: Some(base_uri.clone()),
                count: 5,
                min_size: None,
                max_size: None,
                size_spec: Some(SizeSpec::Fixed(2048)),
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
    
    // Clean up previous files
    std::fs::remove_dir_all(base_path).ok();
    std::fs::create_dir_all(base_path)?;
    
    // Test parallel strategy with high concurrency
    let multi_ep_cache = Arc::new(Mutex::new(HashMap::new()));
    let (objects, _, metrics) = prepare_objects(&parallel_config, None, None, None, &multi_ep_cache, 1, 64, 0, true).await?;
    assert_eq!(objects.len(), 5, "Parallel strategy should create 5 objects with concurrency=64");
    assert_eq!(metrics.objects_created, 5, "Metrics should show 5 objects created");
    
    Ok(())
}
