// S3 Integration Tests
// Tests S3 backend support with s3:// URI scheme
//
// Tests will run if EITHER:
// 1. AWS credentials are configured:
//    - AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY
//    - AWS_PROFILE
//    - IAM role (EC2/ECS/Lambda)
// 2. OR a custom endpoint is configured (for MinIO, LocalStack, etc.):
//    - AWS_ENDPOINT_URL or S3_ENDPOINT_URL
//    (No credentials required for local emulators with anonymous access)
//
// Optionally set S3_BUCKET="your-test-bucket" (defaults to "test")
//
// Run with: cargo test --test s3_tests -- --test-threads=1 --nocapture

use anyhow::{Context, Result};
use bytes::Bytes;
use std::env;

use sai3_bench::workload::{
    create_store_for_uri, BackendType, get_object_no_log, put_object_no_log,
    list_objects_no_log, stat_object_no_log, delete_object_no_log,
};

/// Helper to get S3 test bucket from environment
fn get_s3_bucket() -> Option<String> {
    env::var("S3_BUCKET").ok()
        .or_else(|| env::var("AWS_BUCKET").ok())
}

/// Helper to check if AWS credentials are available
fn has_aws_credentials() -> bool {
    let has_access_key = env::var("AWS_ACCESS_KEY_ID").is_ok();
    let has_secret_key = env::var("AWS_SECRET_ACCESS_KEY").is_ok();
    let has_profile = env::var("AWS_PROFILE").is_ok();
    
    (has_access_key && has_secret_key) || has_profile
}

/// Helper to check if custom endpoint is configured
fn has_custom_endpoint() -> bool {
    env::var("AWS_ENDPOINT_URL").is_ok()
        || env::var("S3_ENDPOINT_URL").is_ok()
}

/// Check if we can run S3 tests - either with credentials OR with a custom endpoint
/// Custom endpoints (MinIO, LocalStack, etc.) may not require real credentials
fn can_run_s3_tests() -> bool {
    has_aws_credentials() || has_custom_endpoint()
}

/// Helper to create S3 test URI
fn s3_test_uri() -> Option<String> {
    let bucket = get_s3_bucket().or_else(|| Some("test".to_string()))?;
    Some(format!("s3://{}/sai3-bench-s3-tests/", bucket))
}

/// Print test configuration info
fn print_test_config() {
    if has_custom_endpoint() {
        let endpoint = env::var("AWS_ENDPOINT_URL")
            .or_else(|_| env::var("S3_ENDPOINT_URL"))
            .unwrap_or_else(|_| "unknown".to_string());
        println!("🔧 Using custom endpoint: {}", endpoint);
    }
    if has_aws_credentials() {
        println!("🔐 Using AWS credentials");
    } else {
        println!("🔓 No credentials (using anonymous/emulator access)");
    }
}

#[test]
fn test_s3_backend_detection() {
    // Test s3:// scheme
    let backend = BackendType::from_uri("s3://my-bucket/prefix/");
    assert!(matches!(backend, BackendType::S3));
    assert_eq!(backend.name(), "S3");
    
    // Test other schemes don't match
    let backend = BackendType::from_uri("az://container/");
    assert!(!matches!(backend, BackendType::S3));
    
    let backend = BackendType::from_uri("gs://bucket/");
    assert!(!matches!(backend, BackendType::S3));
}

#[tokio::test]
async fn test_s3_store_creation() -> Result<()> {
    if !can_run_s3_tests() {
        println!("⚠️  Skipping S3 store creation test - no credentials or custom endpoint");
        println!("   Set AWS_ENDPOINT_URL for local emulator, or");
        println!("   Set AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY for real AWS");
        return Ok(());
    }
    
    let Some(uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    
    println!("🧪 Testing S3 store creation: {}", uri);
    let _store = create_store_for_uri(&uri)
        .context("Failed to create S3 store")?;
    
    println!("✅ Successfully created S3 ObjectStore");
    Ok(())
}

#[tokio::test]
async fn test_s3_put_get_delete() -> Result<()> {
    if !can_run_s3_tests() {
        println!("⚠️  Skipping S3 PUT/GET/DELETE test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing S3 PUT/GET/DELETE cycle");
    
    // Test data (zero-copy: create as Bytes from start)
    let test_key = "test_put_get_delete.txt";
    let test_data = Bytes::from_static(b"Hello from sai3-bench S3 test!");
    let test_uri = format!("{}{}", base_uri, test_key);
    
    // PUT operation
    println!("  📤 PUT: {}", test_uri);
    put_object_no_log(&test_uri, test_data.clone()).await?;
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
    
    println!("✅ S3 PUT/GET/DELETE cycle successful");
    Ok(())
}

#[tokio::test]
async fn test_s3_list_operations() -> Result<()> {
    if !can_run_s3_tests() {
        println!("⚠️  Skipping S3 LIST test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing S3 LIST operations");
    
    // Create test objects (zero-copy: create as Bytes from start)
    let prefix = format!("{}list-test/", base_uri);
    let test_data = Bytes::from_static(b"list test data");
    
    println!("  📤 Creating test objects...");
    for i in 0..5 {
        let uri = format!("{}object-{:03}.txt", prefix, i);
        put_object_no_log(&uri, test_data.clone()).await?;
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
    
    println!("✅ S3 LIST operations successful");
    Ok(())
}

#[tokio::test]
async fn test_s3_stat_operations() -> Result<()> {
    if !can_run_s3_tests() {
        println!("⚠️  Skipping S3 STAT test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing S3 STAT operations");
    
    // Create test object (zero-copy: create as Bytes from start)
    let test_uri = format!("{}stat-test.txt", base_uri);
    let test_data = Bytes::from_static(b"stat test data - 1024 bytes minimum content for size validation");
    
    println!("  📤 Creating test object");
    put_object_no_log(&test_uri, test_data.clone()).await?;
    
    // STAT operation
    println!("  📊 STAT: {}", test_uri);
    let size = stat_object_no_log(&test_uri).await?;
    println!("     ✓ STAT completed: {} bytes", size);
    
    assert_eq!(size, test_data.len() as u64, "Size mismatch");
    
    // Cleanup
    println!("  🗑️  Cleaning up");
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ S3 STAT operations successful");
    Ok(())
}

#[tokio::test]
async fn test_s3_concurrent_operations() -> Result<()> {
    if !can_run_s3_tests() {
        println!("⚠️  Skipping S3 concurrent ops test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing S3 concurrent operations");
    
    let prefix = format!("{}concurrent-test/", base_uri);
    let num_objects = 10;
    let test_data = Bytes::from(vec![0u8; 1024]); // 1KB test data (Bytes::from takes ownership)
    
    // Concurrent PUTs
    println!("  📤 Concurrent PUT: {} objects", num_objects);
    let start = std::time::Instant::now();
    
    let mut handles = vec![];
    for i in 0..num_objects {
        let uri = format!("{}object-{:03}.dat", prefix, i);
        let data = test_data.clone(); // Cheap: just increments refcount
        handles.push(tokio::spawn(async move {
            put_object_no_log(&uri, data).await
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
    
    println!("✅ S3 concurrent operations successful");
    Ok(())
}

#[tokio::test]
async fn test_s3_large_object() -> Result<()> {
    if !can_run_s3_tests() {
        println!("⚠️  Skipping S3 large object test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing S3 large object operations");
    
    let test_uri = format!("{}large-object.bin", base_uri);
    let size_mb = 5;
    let test_data = Bytes::from(vec![0xAB; size_mb * 1024 * 1024]); // 5MB (Bytes::from takes ownership)
    
    println!("  📤 PUT: {} MB object", size_mb);
    let start = std::time::Instant::now();
    put_object_no_log(&test_uri, test_data.clone()).await?;
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
    
    println!("✅ S3 large object test successful");
    Ok(())
}

/// Test custom endpoint configuration for local emulators
#[tokio::test]
async fn test_s3_custom_endpoint() -> Result<()> {
    // This test ONLY runs with a custom endpoint configured
    if !has_custom_endpoint() {
        println!("⚠️  Skipping custom endpoint test - no custom endpoint configured");
        println!("   Set AWS_ENDPOINT_URL or S3_ENDPOINT_URL to enable");
        return Ok(());
    }
    
    let endpoint = env::var("AWS_ENDPOINT_URL")
        .or_else(|_| env::var("S3_ENDPOINT_URL"))
        .unwrap();
    
    println!("🧪 Testing S3 custom endpoint: {}", endpoint);
    print_test_config();
    
    let Some(base_uri) = s3_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    // Test basic operations with custom endpoint
    let test_uri = format!("{}custom-endpoint-test.txt", base_uri);
    let test_data = Bytes::from_static(b"Testing custom S3 endpoint!");
    
    println!("  📤 PUT to custom endpoint");
    put_object_no_log(&test_uri, test_data.clone()).await?;
    
    println!("  📥 GET from custom endpoint");
    let result = get_object_no_log(&test_uri).await?;
    assert_eq!(result, test_data);
    
    println!("  🗑️  DELETE from custom endpoint");
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ S3 custom endpoint test successful");
    Ok(())
}
