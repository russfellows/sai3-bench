// Google Cloud Storage Integration Tests
// Tests GCS backend support with gs:// and gcs:// URI schemes
//
// To run these tests, you need:
// 1. Google Cloud service account credentials
// 2. Environment variable: GOOGLE_APPLICATION_CREDENTIALS="/path/to/key.json"
// 3. Or: GCS_BUCKET="your-test-bucket"
//
// Run with: cargo test --test gcs_tests -- --test-threads=1 --nocapture

use anyhow::{Context, Result};
use std::env;

use sai3_bench::workload::{
    create_store_for_uri, BackendType, get_object_no_log, put_object_no_log,
    list_objects_no_log, stat_object_no_log, delete_object_no_log,
};

/// Helper to get GCS test bucket from environment
fn get_gcs_bucket() -> Option<String> {
    env::var("GCS_BUCKET").ok()
        .or_else(|| env::var("GOOGLE_CLOUD_BUCKET").ok())
}

/// Helper to check if GCS credentials are available
fn has_gcs_credentials() -> bool {
    env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok() 
        || env::var("GCS_BUCKET").is_ok()
        || env::var("GOOGLE_CLOUD_PROJECT").is_ok()
}

/// Helper to create GCS test URI
fn gcs_test_uri() -> Option<String> {
    get_gcs_bucket().map(|bucket| {
        format!("gs://{}/io-bench-gcs-tests/", bucket)
    })
}

#[test]
fn test_gcs_backend_detection() {
    // Test gs:// scheme
    let backend = BackendType::from_uri("gs://my-bucket/prefix/");
    assert!(matches!(backend, BackendType::Gcs));
    assert_eq!(backend.name(), "Google Cloud Storage");
    
    // Test gcs:// scheme (alternate)
    let backend = BackendType::from_uri("gcs://my-bucket/prefix/");
    assert!(matches!(backend, BackendType::Gcs));
    
    // Test other schemes don't match
    let backend = BackendType::from_uri("s3://bucket/");
    assert!(!matches!(backend, BackendType::Gcs));
}

#[tokio::test]
async fn test_gcs_store_creation() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS store creation test - no credentials");
        return Ok(());
    }
    
    let Some(uri) = gcs_test_uri() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS store creation: {}", uri);
    let _store = create_store_for_uri(&uri)
        .context("Failed to create GCS store")?;
    
    println!("✅ Successfully created GCS ObjectStore");
    Ok(())
}

#[tokio::test]
async fn test_gcs_put_get_delete() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS PUT/GET/DELETE test - no credentials");
        return Ok(());
    }
    
    let Some(base_uri) = gcs_test_uri() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS PUT/GET/DELETE cycle");
    
    // Test data
    let test_key = "test_put_get_delete.txt";
    let test_data = b"Hello from sai3-bench GCS test!";
    let test_uri = format!("{}{}", base_uri, test_key);
    
    // PUT operation
    println!("  📤 PUT: {}", test_uri);
    put_object_no_log(&test_uri, test_data).await?;
    println!("     ✓ PUT completed: {} bytes", test_data.len());
    
    // GET operation
    println!("  📥 GET: {}", test_uri);
    let retrieved_data = get_object_no_log(&test_uri).await?;
    println!("     ✓ GET completed: {} bytes", retrieved_data.len());
    
    // Verify content
    assert_eq!(retrieved_data, test_data, "Retrieved data doesn't match");
    println!("     ✓ Content verified");
    
    // DELETE operation
    println!("  🗑️  DELETE: {}", test_uri);
    delete_object_no_log(&test_uri).await?;
    println!("     ✓ DELETE completed");
    
    println!("✅ GCS PUT/GET/DELETE cycle successful");
    Ok(())
}

#[tokio::test]
async fn test_gcs_list_operations() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS LIST test - no credentials");
        return Ok(());
    }
    
    let Some(base_uri) = gcs_test_uri() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS LIST operations");
    
    // Create test objects
    let prefix = format!("{}list-test/", base_uri);
    let test_data = b"list test data";
    
    println!("  📤 Creating test objects...");
    for i in 0..5 {
        let uri = format!("{}object-{:03}.txt", prefix, i);
        put_object_no_log(&uri, test_data).await?;
    }
    println!("     ✓ Created 5 test objects");
    
    // LIST operation
    println!("  📋 LIST: {}", prefix);
    let objects = list_objects_no_log(&prefix).await?;
    println!("     ✓ LIST completed: {} objects", objects.len());
    
    assert!(objects.len() >= 5, "Expected at least 5 objects");
    
    // Cleanup
    println!("  🗑️  Cleaning up test objects...");
    for i in 0..5 {
        let uri = format!("{}object-{:03}.txt", prefix, i);
        delete_object_no_log(&uri).await?;
    }
    
    println!("✅ GCS LIST operations successful");
    Ok(())
}

#[tokio::test]
async fn test_gcs_stat_operations() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS STAT test - no credentials");
        return Ok(());
    }
    
    let Some(base_uri) = gcs_test_uri() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS STAT operations");
    
    // Create test object
    let test_uri = format!("{}stat-test.txt", base_uri);
    let test_data = b"stat test data - 1024 bytes minimum content for size validation";
    
    println!("  📤 Creating test object");
    put_object_no_log(&test_uri, test_data).await?;
    
    // STAT operation
    println!("  📊 STAT: {}", test_uri);
    let size = stat_object_no_log(&test_uri).await?;
    println!("     ✓ STAT completed: {} bytes", size);
    
    assert_eq!(size, test_data.len() as u64, "Size mismatch");
    
    // Cleanup
    println!("  🗑️  Cleaning up");
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ GCS STAT operations successful");
    Ok(())
}

#[tokio::test]
async fn test_gcs_concurrent_operations() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS concurrent ops test - no credentials");
        return Ok(());
    }
    
    let Some(base_uri) = gcs_test_uri() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS concurrent operations");
    
    let prefix = format!("{}concurrent-test/", base_uri);
    let num_objects = 10;
    let test_data = vec![0u8; 1024]; // 1KB test data
    
    // Concurrent PUTs
    println!("  📤 Concurrent PUT: {} objects", num_objects);
    let start = std::time::Instant::now();
    
    let mut handles = vec![];
    for i in 0..num_objects {
        let uri = format!("{}object-{:03}.dat", prefix, i);
        let data = test_data.clone();
        handles.push(tokio::spawn(async move {
            put_object_no_log(&uri, &data).await
        }));
    }
    
    for handle in handles {
        handle.await.unwrap()?;
    }
    
    let put_duration = start.elapsed();
    println!("     ✓ PUT completed in {:?}", put_duration);
    
    // Concurrent GETs
    println!("  📥 Concurrent GET: {} objects", num_objects);
    let start = std::time::Instant::now();
    
    let mut handles = vec![];
    for i in 0..num_objects {
        let uri = format!("{}object-{:03}.dat", prefix, i);
        handles.push(tokio::spawn(async move {
            get_object_no_log(&uri).await
        }));
    }
    
    for handle in handles {
        let result = handle.await.unwrap()?;
        assert_eq!(result.len(), 1024);
    }
    
    let get_duration = start.elapsed();
    println!("     ✓ GET completed in {:?}", get_duration);
    
    // Cleanup
    println!("  🗑️  Cleaning up {} objects", num_objects);
    for i in 0..num_objects {
        let uri = format!("{}object-{:03}.dat", prefix, i);
        delete_object_no_log(&uri).await?;
    }
    
    println!("✅ GCS concurrent operations successful");
    Ok(())
}

#[tokio::test]
async fn test_gcs_large_object() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS large object test - no credentials");
        return Ok(());
    }
    
    let Some(base_uri) = gcs_test_uri() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS large object operations");
    
    let test_uri = format!("{}large-object.bin", base_uri);
    let size_mb = 5;
    let test_data = vec![0xAB; size_mb * 1024 * 1024]; // 5MB
    
    println!("  📤 PUT: {} MB object", size_mb);
    let start = std::time::Instant::now();
    put_object_no_log(&test_uri, &test_data).await?;
    let put_duration = start.elapsed();
    println!("     ✓ PUT completed in {:?} ({:.2} MB/s)", 
             put_duration, 
             size_mb as f64 / put_duration.as_secs_f64());
    
    println!("  📥 GET: {} MB object", size_mb);
    let start = std::time::Instant::now();
    let retrieved_data = get_object_no_log(&test_uri).await?;
    let get_duration = start.elapsed();
    println!("     ✓ GET completed in {:?} ({:.2} MB/s)", 
             get_duration,
             size_mb as f64 / get_duration.as_secs_f64());
    
    assert_eq!(retrieved_data.len(), test_data.len());
    assert_eq!(retrieved_data, test_data);
    
    println!("  🗑️  DELETE: {} MB object", size_mb);
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ GCS large object test successful");
    Ok(())
}

#[tokio::test]
async fn test_gcs_alternate_scheme() -> Result<()> {
    if !has_gcs_credentials() {
        println!("⚠️  Skipping GCS alternate scheme test - no credentials");
        return Ok(());
    }
    
    let Some(bucket) = get_gcs_bucket() else {
        println!("⚠️  Skipping test - GCS_BUCKET not set");
        return Ok(());
    };
    
    println!("🧪 Testing GCS alternate URI schemes");
    
    // Test both gs:// and gcs:// schemes
    let gs_uri = format!("gs://{}/io-bench-test/scheme-test.txt", bucket);
    let gcs_uri = format!("gcs://{}/io-bench-test/scheme-test.txt", bucket);
    
    let test_data = b"testing alternate schemes";
    
    // PUT with gs://
    println!("  📤 PUT with gs:// scheme");
    put_object_no_log(&gs_uri, test_data).await?;
    
    // GET with gcs:// (should work - same object)
    println!("  📥 GET with gcs:// scheme");
    let result = get_object_no_log(&gcs_uri).await?;
    assert_eq!(result, test_data);
    
    // Cleanup
    delete_object_no_log(&gs_uri).await?;
    
    println!("✅ GCS alternate scheme test successful");
    Ok(())
}
