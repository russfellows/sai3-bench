// Azure Blob Storage Integration Tests
// Tests Azure backend support with az:// and azure:// URI schemes
//
// Tests will run if EITHER:
// 1. Azure credentials are configured:
//    - AZURE_STORAGE_ACCOUNT + AZURE_STORAGE_KEY
//    - AZURE_STORAGE_ACCOUNT + AZURE_STORAGE_SAS_TOKEN
//    - AZURE_STORAGE_CONNECTION_STRING
// 2. OR a custom endpoint is configured (for local emulators like Azurite):
//    - AZURE_STORAGE_ENDPOINT or AZURE_BLOB_ENDPOINT_URL
//    (No credentials required for local emulators)
//
// Optionally set AZURE_CONTAINER="your-test-container" (defaults to "test")
//
// Run with: cargo test --test azure_tests -- --test-threads=1 --nocapture

use anyhow::{Context, Result};
use bytes::Bytes;
use std::env;

use sai3_bench::workload::{
    create_store_for_uri, BackendType, get_object_no_log, put_object_no_log,
    list_objects_no_log, stat_object_no_log, delete_object_no_log,
};

/// Helper to get Azure test container from environment
fn get_azure_container() -> Option<String> {
    env::var("AZURE_CONTAINER").ok()
        .or_else(|| env::var("AZURE_STORAGE_CONTAINER").ok())
}

/// Helper to check if Azure credentials are available
fn has_azure_credentials() -> bool {
    let has_account = env::var("AZURE_STORAGE_ACCOUNT").is_ok();
    let has_key = env::var("AZURE_STORAGE_KEY").is_ok();
    let has_sas = env::var("AZURE_STORAGE_SAS_TOKEN").is_ok();
    let has_connection_string = env::var("AZURE_STORAGE_CONNECTION_STRING").is_ok();
    
    // Need account + (key or sas or connection string)
    has_account && (has_key || has_sas || has_connection_string)
        || has_connection_string
}

/// Helper to check if custom endpoint is configured
fn has_custom_endpoint() -> bool {
    env::var("AZURE_STORAGE_ENDPOINT").is_ok()
        || env::var("AZURE_BLOB_ENDPOINT_URL").is_ok()
}

/// Check if we can run Azure tests - either with credentials OR with a custom endpoint
/// Custom endpoints (Azurite, local emulators) typically don't require real credentials
fn can_run_azure_tests() -> bool {
    has_azure_credentials() || has_custom_endpoint()
}

/// Helper to create Azure test URI
fn azure_test_uri() -> Option<String> {
    let container = get_azure_container().or_else(|| Some("test".to_string()))?;
    Some(format!("az://{}/sai3-bench-azure-tests/", container))
}

/// Print test configuration info
fn print_test_config() {
    if has_custom_endpoint() {
        let endpoint = env::var("AZURE_STORAGE_ENDPOINT")
            .or_else(|_| env::var("AZURE_BLOB_ENDPOINT_URL"))
            .unwrap_or_else(|_| "unknown".to_string());
        println!("🔧 Using custom endpoint: {}", endpoint);
    }
    if has_azure_credentials() {
        println!("🔐 Using Azure credentials");
    } else {
        println!("🔓 No credentials (using anonymous/emulator access)");
    }
}

#[test]
fn test_azure_backend_detection() {
    // Test az:// scheme
    let backend = BackendType::from_uri("az://my-container/prefix/");
    assert!(matches!(backend, BackendType::Azure));
    assert_eq!(backend.name(), "Azure Blob");
    
    // Test azure:// scheme (alternate)
    let backend = BackendType::from_uri("azure://my-container/prefix/");
    assert!(matches!(backend, BackendType::Azure));
    
    // Test other schemes don't match
    let backend = BackendType::from_uri("s3://bucket/");
    assert!(!matches!(backend, BackendType::Azure));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_store_creation() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure store creation test - no credentials or custom endpoint");
        println!("   Set AZURE_STORAGE_ENDPOINT for local emulator, or");
        println!("   Set AZURE_STORAGE_ACCOUNT + AZURE_STORAGE_KEY for real Azure");
        return Ok(());
    }
    
    let Some(uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    
    println!("🧪 Testing Azure store creation: {}", uri);
    let _store = create_store_for_uri(&uri)
        .context("Failed to create Azure store")?;
    
    println!("✅ Successfully created Azure ObjectStore");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_put_get_delete() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure PUT/GET/DELETE test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing Azure PUT/GET/DELETE cycle");
    
    // Test data (zero-copy: create as Bytes from start)
    let test_key = "test_put_get_delete.txt";
    let test_data = Bytes::from_static(b"Hello from sai3-bench Azure test!");
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
    
    println!("✅ Azure PUT/GET/DELETE cycle successful");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_list_operations() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure LIST test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing Azure LIST operations");
    
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
    
    println!("✅ Azure LIST operations successful");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_stat_operations() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure STAT test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing Azure STAT operations");
    
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
    
    println!("✅ Azure STAT operations successful");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_concurrent_operations() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure concurrent ops test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing Azure concurrent operations");
    
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
    
    println!("✅ Azure concurrent operations successful");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_large_object() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure large object test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let Some(base_uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    print_test_config();
    println!("🧪 Testing Azure large object operations");
    
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
    
    println!("✅ Azure large object test successful");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_azure_alternate_scheme() -> Result<()> {
    if !can_run_azure_tests() {
        println!("⚠️  Skipping Azure alternate scheme test - no credentials or custom endpoint");
        return Ok(());
    }
    
    let container = get_azure_container().unwrap_or_else(|| "test".to_string());
    
    print_test_config();
    println!("🧪 Testing Azure az:// scheme with multiple paths");
    
    // Test az:// scheme with different paths
    let uri1 = format!("az://{}/sai3-bench-test/scheme-test-1.txt", container);
    let uri2 = format!("az://{}/sai3-bench-test/scheme-test-2.txt", container);
    
    let test_data1 = Bytes::from_static(b"testing scheme path 1");
    let test_data2 = Bytes::from_static(b"testing scheme path 2");
    
    // PUT first object
    println!("  📤 PUT first object");
    put_object_no_log(&uri1, test_data1.clone()).await?;
    
    // PUT second object  
    println!("  📤 PUT second object");
    put_object_no_log(&uri2, test_data2.clone()).await?;
    
    // GET both back
    println!("  📥 GET first object");
    let result1 = get_object_no_log(&uri1).await?;
    assert_eq!(result1, test_data1);
    
    println!("  📥 GET second object");
    let result2 = get_object_no_log(&uri2).await?;
    assert_eq!(result2, test_data2);
    
    // Cleanup
    delete_object_no_log(&uri1).await?;
    delete_object_no_log(&uri2).await?;
    
    println!("✅ Azure scheme test successful");
    Ok(())
}

/// Test that custom endpoint mode works (specifically for local emulators)
#[tokio::test(flavor = "multi_thread")]
async fn test_azure_custom_endpoint() -> Result<()> {
    // This test ONLY runs with a custom endpoint configured
    if !has_custom_endpoint() {
        println!("⚠️  Skipping custom endpoint test - no custom endpoint configured");
        println!("   Set AZURE_STORAGE_ENDPOINT or AZURE_BLOB_ENDPOINT_URL to enable");
        return Ok(());
    }
    
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")
        .or_else(|_| env::var("AZURE_BLOB_ENDPOINT_URL"))
        .unwrap();
    
    println!("🧪 Testing Azure custom endpoint: {}", endpoint);
    print_test_config();
    
    let Some(base_uri) = azure_test_uri() else {
        println!("⚠️  Skipping test - could not construct test URI");
        return Ok(());
    };
    
    // Test basic operations with custom endpoint
    let test_uri = format!("{}custom-endpoint-test.txt", base_uri);
    let test_data = Bytes::from_static(b"Testing custom Azure endpoint!");
    
    println!("  📤 PUT to custom endpoint");
    put_object_no_log(&test_uri, test_data.clone()).await?;
    
    println!("  📥 GET from custom endpoint");
    let result = get_object_no_log(&test_uri).await?;
    assert_eq!(result, test_data);
    
    println!("  🗑️  DELETE from custom endpoint");
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ Azure custom endpoint test successful");
    Ok(())
}
