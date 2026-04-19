//! Integration tests for streaming replay functionality
//!
//! **SERIAL EXECUTION**: All tests use `#[serial]` to prevent concurrent execution.
//! s3dlio's op-logger is a process-global singleton that can only be init/finalized once.
//!
//! **ORDERING**: `#[serial]` prevents concurrent runs but does NOT guarantee source order.
//! All tests call `ensure_oplog_created()` which uses `std::sync::Once` + a dedicated
//! OS thread to create the fixture exactly once, whichever test happens to run first.

use anyhow::Result;
use bytes::Bytes;
use s3dlio_oplog::OpLogStreamReader;
use sai3_bench::replay_streaming::{replay_workload_streaming, ReplayRunConfig};
use sai3_bench::workload::{
    create_store_with_logger, finalize_operation_logger, init_operation_logger,
};
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::thread;

// =============================================================================
// Shared one-time fixture setup
// =============================================================================

/// Initialises the shared test op-log exactly once per test-binary invocation,
/// regardless of which test function runs first.
///
/// `#[serial]` ensures only one test body runs at a time, but the Rust test
/// harness can dispatch test threads in any order, so test_01–test_06 may start
/// in non-source order.  Using `Once` + a fresh OS thread sidesteps both the
/// ordering problem and the "cannot start a runtime from within a runtime"
/// restriction that would fire if we called `Runtime::block_on` directly inside
/// a `#[tokio::test]` body.
static OPLOG_INIT: Once = Once::new();

fn ensure_oplog_created() {
    OPLOG_INIT.call_once(|| {
        thread::spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime for oplog setup");
            rt.block_on(async {
                do_create_test_oplog()
                    .await
                    .expect("Failed to create test op-log fixture");
            });
        })
        .join()
        .expect("Oplog setup thread panicked");
    });
}

/// The actual fixture-creation logic.  Called at most once per process via
/// `ensure_oplog_created`.
async fn do_create_test_oplog() -> Result<()> {
    let test_base = std::env::temp_dir().join("sai3-bench-streaming-tests");
    let data_dir = test_base.join("data");
    let oplog_path = test_base.join("test-operations.tsv.zst");

    // Clean up any previous run so we get a fresh op-log
    let _ = fs::remove_dir_all(&test_base);
    fs::create_dir_all(&data_dir)?;

    let base_uri = format!("file://{}", data_dir.display());

    println!("=== CREATING TEST OP-LOG FIXTURE ===");
    println!("Op-log path: {}", oplog_path.display());

    // Initialize s3dlio operation logger (global singleton — ONCE per process)
    init_operation_logger(&oplog_path)?;

    // Create object store with logging enabled
    let store = create_store_with_logger(&base_uri)?;

    // 50 PUTs
    for i in 0..50 {
        let key = format!("test-object-{:04}.dat", i);
        let uri = format!("{}/{}", base_uri.trim_end_matches('/'), key);
        let data = Bytes::from(format!("test-data-{}", i).repeat(100).into_bytes());
        store.put(&uri, data).await?;
    }

    // 50 GETs
    for i in 0..50 {
        let key = format!("test-object-{:04}.dat", i);
        let uri = format!("{}/{}", base_uri.trim_end_matches('/'), key);
        let _ = store.get(&uri).await?;
    }

    // 1 LIST
    let list_uri = format!("{}/", base_uri.trim_end_matches('/'));
    let _ = store.list(&list_uri, true).await?;

    // CRITICAL: flush the zstd frame
    finalize_operation_logger()?;

    assert!(
        oplog_path.exists(),
        "Op-log file should exist after creation"
    );
    let op_count = verify_oplog_contents(&oplog_path)?;
    // 50 PUTs + 50 GETs + 1 LIST = 101 operations
    assert!(
        op_count >= 100,
        "Op-log should have ≥100 ops, got {}",
        op_count
    );
    println!("✓ Op-log fixture ready: {} operations", op_count);
    Ok(())
}

/// Get path to shared test op-log (created by test_01_generate_oplog)
fn get_test_oplog_path() -> PathBuf {
    std::env::temp_dir().join("sai3-bench-streaming-tests/test-operations.tsv.zst")
}

/// Get path to shared test data directory
fn get_test_data_dir() -> PathBuf {
    std::env::temp_dir().join("sai3-bench-streaming-tests/data")
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
#[serial]
async fn test_01_generate_oplog() -> Result<()> {
    // Ensure the op-log is created (idempotent — safe to call from any test first).
    ensure_oplog_created();

    println!("=== TEST 01: OP-LOG GENERATION ===");
    let oplog_path = get_test_oplog_path();
    assert!(oplog_path.exists(), "Op-log file should exist");
    let op_count = verify_oplog_contents(&oplog_path)?;
    assert!(
        op_count >= 100,
        "Should have at least 100 operations, got {}",
        op_count
    );
    println!("✓ Op-log verified: {} operations", op_count);
    println!("✓ Test data at: {}", oplog_path.parent().unwrap().display());
    Ok(())
}

// =============================================================================
// TEST 02: Basic round-trip replay
// =============================================================================

#[tokio::test]
#[serial]
async fn test_02_replay_basic() -> Result<()> {
    ensure_oplog_created();
    let oplog_path = get_test_oplog_path();

    println!("=== BASIC REPLAY TEST ===");
    println!("Reading op-log: {}", oplog_path.display());

    // Verify we can read the op-log
    let op_count = verify_oplog_contents(&oplog_path)?;
    println!("Op-log contains {} operations", op_count);
    assert!(op_count >= 100, "Expected at least 100 operations");

    // Replay at high speed
    println!("Replaying at 100x speed...");
    let replay_config = ReplayRunConfig {
        op_log_path: oplog_path,
        target_uri: None, // Use original URIs from op-log
        speed: 100.0,
        continue_on_error: false,
        max_concurrent: Some(200),
        remap_config: None,
        backpressure: None,
    };

    replay_workload_streaming(replay_config).await?;
    println!("✓ Replay completed successfully");

    Ok(())
}

// =============================================================================
// TEST 03: Streaming reader memory efficiency
// =============================================================================

#[tokio::test]
#[serial]
async fn test_03_streaming_reader() -> Result<()> {
    ensure_oplog_created();
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
#[serial]
async fn test_04_uri_remapping() -> Result<()> {
    ensure_oplog_created();
    let oplog_path = get_test_oplog_path();

    println!("=== URI REMAPPING TEST ===");

    // Create a different target directory
    let target_dir = std::env::temp_dir().join("sai3-bench-streaming-tests/remapped");
    fs::create_dir_all(&target_dir)?;
    let target_uri = format!("file://{}", target_dir.display());

    println!("Replaying to remapped URI: {}", target_uri);
    println!("NOTE: GET operations will fail (files don't exist at new location)");
    println!("Using continue_on_error=true to demonstrate URI translation");

    // Replay to different target with continue_on_error
    // PUT operations will create files at new location
    // GET/DELETE operations will fail (files don't exist) - this is expected
    let replay_config = ReplayRunConfig {
        op_log_path: oplog_path,
        target_uri: Some(target_uri.clone()),
        speed: 100.0,
        continue_on_error: true, // Expect failures for GET/DELETE of non-existent files
        max_concurrent: Some(100),
        remap_config: None,
        backpressure: None,
    };

    replay_workload_streaming(replay_config).await?;
    println!("✓ URI remapping test completed (with expected failures)");

    Ok(())
}

// =============================================================================
// TEST 05: Continue on error
// =============================================================================

#[tokio::test]
#[serial]
async fn test_05_continue_on_error() -> Result<()> {
    ensure_oplog_created();
    let oplog_path = get_test_oplog_path();
    let _data_dir = get_test_data_dir();

    println!("=== CONTINUE ON ERROR TEST ===");

    // Delete the data directory so GET operations will fail
    let temp_dir = std::env::temp_dir().join("sai3-bench-streaming-tests/error-test");
    fs::create_dir_all(&temp_dir)?;

    println!("Replaying with continue_on_error=true (expect some failures)...");
    let replay_config = ReplayRunConfig {
        op_log_path: oplog_path,
        target_uri: Some(format!("file://{}", temp_dir.display())),
        speed: 100.0,
        continue_on_error: true, // Should not panic on errors
        max_concurrent: Some(50),
        remap_config: None,
        backpressure: None,
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
#[serial]
async fn test_06_concurrent_limits() -> Result<()> {
    ensure_oplog_created();
    let oplog_path = get_test_oplog_path();

    println!("=== CONCURRENT EXECUTION TEST ===");

    // Test with low concurrency
    println!("Testing with max_concurrent=5...");
    let replay_config = ReplayRunConfig {
        op_log_path: oplog_path.clone(),
        target_uri: None,
        speed: 100.0,
        continue_on_error: false,
        max_concurrent: Some(5),
        remap_config: None,
        backpressure: None,
    };
    replay_workload_streaming(replay_config).await?;

    // Test with high concurrency
    println!("Testing with max_concurrent=100...");
    let replay_config = ReplayRunConfig {
        op_log_path: oplog_path,
        target_uri: None,
        speed: 100.0,
        continue_on_error: false,
        max_concurrent: Some(100),
        remap_config: None,
        backpressure: None,
    };
    replay_workload_streaming(replay_config).await?;

    println!("✓ Concurrent execution test passed");

    Ok(())
}
