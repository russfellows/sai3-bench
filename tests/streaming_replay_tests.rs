/// Integration tests for streaming replay functionality
///
/// **TEST STRUCTURE**: To work around s3dlio's global singleton op-logger limitation:
/// 1. `test_01_generate_oplog` - Creates op-log files (calls finalize once)
/// 2. Other tests - Read and replay existing op-logs (no logging)
///
/// This ensures the global logger is only initialized/finalized once per test run.
/// Tests are numbered to enforce execution order with `--test-threads=1`.

use anyhow::Result;
use io_bench::replay_streaming::{replay_workload_streaming, ReplayConfig};
use io_bench::workload::{init_operation_logger, finalize_operation_logger, create_store_with_logger};
use s3dlio_oplog::OpLogStreamReader;
use std::fs;
use std::path::{Path, PathBuf};

/// Get path to shared test op-log (created by test_01_generate_oplog)
fn get_test_oplog_path() -> PathBuf {
    std::env::temp_dir().join("io-bench-streaming-tests/test-operations.tsv.zst")
}

/// Get path to shared test data directory
fn get_test_data_dir() -> PathBuf {
    std::env::temp_dir().join("io-bench-streaming-tests/data")
}

/// Verify op-log contents using streaming reader
fn verify_oplog_contents(oplog_path: &Path) -> Result<usize> {
    let stream = OpLogStreamReader::from_file(oplog_path)?;
    
    let mut count = 0;
    for entry_result in stream {
        let _entry = entry_result?;
        count += 1;
    }
    
    Ok(count)
}

// =============================================================================
// TEST 01: Generate op-log (MUST RUN FIRST)
// =============================================================================

#[tokio::test]
async fn test_01_generate_oplog() -> Result<()> {
    // Create persistent test directories (not TempDir - we want them to survive)
    let test_base = std::env::temp_dir().join("io-bench-streaming-tests");
    let data_dir = test_base.join("data");
    let oplog_path = test_base.join("test-operations.tsv.zst");
    
    // Clean up any previous test run
    let _ = fs::remove_dir_all(&test_base);
    fs::create_dir_all(&data_dir)?;
    
    let base_uri = format!("file://{}", data_dir.display());
    
    println!("=== GENERATING TEST OP-LOG ===");
    println!("Op-log path: {}", oplog_path.display());
    println!("Data directory: {}", data_dir.display());
    
    // Initialize s3dlio operation logger (ONCE per process)
    init_operation_logger(&oplog_path)?;
    
    // Create object store with logging enabled
    let store = create_store_with_logger(&base_uri)?;
    
    // Generate 50 test objects with operations
    println!("Creating 50 test objects...");
    for i in 0..50 {
        let key = format!("test-object-{:04}.dat", i);
        let uri = format!("{}/{}", base_uri.trim_end_matches('/'), key);
        let data = format!("test-data-{}", i).repeat(100); // ~1KB per object
        store.put(&uri, data.as_bytes()).await?;
    }
    
    // Perform GET operations on all objects
    println!("Reading all 50 objects...");
    for i in 0..50 {
        let key = format!("test-object-{:04}.dat", i);
        let uri = format!("{}/{}", base_uri.trim_end_matches('/'), key);
        let _ = store.get(&uri).await?;
    }
    
    // Perform LIST operation
    println!("Listing objects...");
    let list_uri = format!("{}/", base_uri.trim_end_matches('/'));
    let _ = store.list(&list_uri, true).await?;
    
    // CRITICAL: Finalize to flush zstd stream
    println!("Finalizing op-log...");
    finalize_operation_logger()?;
    
    // Verify op-log was created
    assert!(oplog_path.exists(), "Op-log file should exist");
    let op_count = verify_oplog_contents(&oplog_path)?;
    
    // 50 PUTs + 50 GETs + 1 LIST = 101 operations
    assert!(op_count >= 100, "Should have at least 100 operations, got {}", op_count);
    println!("✓ Op-log created successfully: {} operations", op_count);
    println!("✓ Test data persisted to: {}", test_base.display());
    
    Ok(())
}

// =============================================================================
// TEST 02: Basic round-trip replay
// =============================================================================

#[tokio::test]
async fn test_02_replay_basic() -> Result<()> {
    let oplog_path = get_test_oplog_path();
    
    println!("=== BASIC REPLAY TEST ===");
    println!("Reading op-log: {}", oplog_path.display());
    
    // Verify we can read the op-log
    let op_count = verify_oplog_contents(&oplog_path)?;
    println!("Op-log contains {} operations", op_count);
    assert!(op_count >= 100, "Expected at least 100 operations");
    
    // Replay at high speed
    println!("Replaying at 100x speed...");
        let replay_config = ReplayConfig {
        op_log_path: oplog_path.clone(),
        target_uri: Some(replay_target.clone()),
        speed: 1.0,
        continue_on_error: false,
        max_concurrent: Some(100),
        remap_config: None,
        remap_config: None,
    };
    
    replay_workload_streaming(replay_config).await?;
    println!("✓ Replay completed successfully");
    
    Ok(())
}

// =============================================================================
// TEST 03: Streaming reader memory efficiency
// =============================================================================

#[tokio::test]
async fn test_03_streaming_reader() -> Result<()> {
    let oplog_path = get_test_oplog_path();
    
    println!("=== STREAMING READER TEST ===");
    println!("Processing op-log with streaming reader...");
    
    // Use streaming reader to count operations without loading all into memory
    let stream = OpLogStreamReader::from_file(&oplog_path)?;
    
    let mut total = 0;
    for entry_result in stream {
        let _entry = entry_result?;
        total += 1;
    }
    
    println!("✓ Processed {} operations with constant memory", total);
    assert!(total >= 100, "Should have processed many operations");
    
    Ok(())
}

// =============================================================================
// TEST 04: URI remapping
// =============================================================================

#[tokio::test]
async fn test_04_uri_remapping() -> Result<()> {
    let oplog_path = get_test_oplog_path();
    
    println!("=== URI REMAPPING TEST ===");
    
    // Create a different target directory
    let target_dir = std::env::temp_dir().join("io-bench-streaming-tests/remapped");
    fs::create_dir_all(&target_dir)?;
    let target_uri = format!("file://{}", target_dir.display());
    
    println!("Replaying to remapped URI: {}", target_uri);
    println!("NOTE: GET operations will fail (files don't exist at new location)");
    println!("Using continue_on_error=true to demonstrate URI translation");
    
    // Replay to different target with continue_on_error
    // PUT operations will create files at new location
    // GET/DELETE operations will fail (files don't exist) - this is expected
    let replay_config = ReplayConfig {
        op_log_path: oplog_path,
        target_uri: Some(target_uri.clone()),
        speed: 100.0,
        continue_on_error: true, // Expect failures for GET/DELETE of non-existent files
        max_concurrent: Some(100),
    };
    
    replay_workload_streaming(replay_config).await?;
    println!("✓ URI remapping test completed (with expected failures)");
    
    Ok(())
}

// =============================================================================
// TEST 05: Continue on error
// =============================================================================

#[tokio::test]
async fn test_05_continue_on_error() -> Result<()> {
    let oplog_path = get_test_oplog_path();
    let _data_dir = get_test_data_dir();
    
    println!("=== CONTINUE ON ERROR TEST ===");
    
    // Delete the data directory so GET operations will fail
    let temp_dir = std::env::temp_dir().join("io-bench-streaming-tests/error-test");
    fs::create_dir_all(&temp_dir)?;
    
    println!("Replaying with continue_on_error=true (expect some failures)...");
    let replay_config = ReplayConfig {
        op_log_path: oplog_path,
        target_uri: Some(format!("file://{}", temp_dir.display())),
        speed: 100.0,
        continue_on_error: true, // Should not panic on errors
        max_concurrent: Some(50),
    };
    
    // Should complete despite errors
    replay_workload_streaming(replay_config).await?;
    println!("✓ Error handling test passed");
    
    Ok(())
}

// =============================================================================
// TEST 06: Concurrent execution limits
// =============================================================================

#[tokio::test]
async fn test_06_concurrent_limits() -> Result<()> {
    let oplog_path = get_test_oplog_path();
    
    println!("=== CONCURRENT EXECUTION TEST ===");
    
    // Test with low concurrency
    println!("Testing with max_concurrent=5...");
    let replay_config = ReplayConfig {
        op_log_path: oplog_path.clone(),
        target_uri: None,
        speed: 100.0,
        continue_on_error: false,
        max_concurrent: Some(5),
    };
    replay_workload_streaming(replay_config).await?;
    
    // Test with high concurrency
    println!("Testing with max_concurrent=100...");
    let replay_config = ReplayConfig {
        op_log_path: oplog_path,
        target_uri: None,
        speed: 100.0,
        continue_on_error: false,
        max_concurrent: Some(100),
    };
    replay_workload_streaming(replay_config).await?;
    
    println!("✓ Concurrent execution test passed");
    
    Ok(())
}
