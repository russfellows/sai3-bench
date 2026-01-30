// Local File Backend Integration Tests
// Tests file:// backend support for local filesystem operations
//
// These tests always run - no credentials needed for local filesystem.
// Uses temporary directories for isolation.
//
// Run with: cargo test --test file_tests -- --test-threads=1 --nocapture

use anyhow::{Context, Result};
use bytes::Bytes;
use tempfile::TempDir;

use sai3_bench::workload::{
    create_store_for_uri, BackendType, get_object_no_log, put_object_no_log,
    list_objects_no_log, stat_object_no_log, delete_object_no_log,
};

/// Helper to create a temp directory and return file:// URI
fn create_test_dir() -> Result<(TempDir, String)> {
    let temp_dir = TempDir::new()?;
    let uri = format!("file://{}/", temp_dir.path().display());
    Ok((temp_dir, uri))
}

#[test]
fn test_file_backend_detection() {
    // Test file:// scheme
    let backend = BackendType::from_uri("file:///tmp/test/");
    assert!(matches!(backend, BackendType::File));
    assert_eq!(backend.name(), "Local File");
    
    // Test other schemes don't match
    let backend = BackendType::from_uri("s3://bucket/");
    assert!(!matches!(backend, BackendType::File));
    
    let backend = BackendType::from_uri("az://container/");
    assert!(!matches!(backend, BackendType::File));
}

#[tokio::test]
async fn test_file_store_creation() -> Result<()> {
    let (_temp_dir, uri) = create_test_dir()?;
    
    println!("🧪 Testing file store creation: {}", uri);
    let _store = create_store_for_uri(&uri)
        .context("Failed to create file store")?;
    
    println!("✅ Successfully created file ObjectStore");
    Ok(())
}

#[tokio::test]
async fn test_file_put_get_delete() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file PUT/GET/DELETE cycle");
    
    // Test data (zero-copy: create as Bytes from start)
    let test_key = "test_put_get_delete.txt";
    let test_data = Bytes::from_static(b"Hello from sai3-bench file test!");
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
    
    // Verify deletion
    let result = get_object_no_log(&test_uri).await;
    assert!(result.is_err(), "File should be deleted");
    println!("     ✓ Deletion verified");
    
    println!("✅ File PUT/GET/DELETE cycle successful");
    Ok(())
}

#[tokio::test]
async fn test_file_list_operations() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file LIST operations");
    
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
    
    assert_eq!(objects.len(), 5, "Expected exactly 5 objects");
    
    // Verify objects are in the list
    for i in 0..5 {
        let expected_name = format!("object-{:03}.txt", i);
        assert!(
            objects.iter().any(|o| o.ends_with(&expected_name)),
            "Missing object: {}", expected_name
        );
    }
    println!("     ✓ All objects found in listing");
    
    // Cleanup
    println!("  🗑️  Cleaning up test objects...");
    for i in 0..5 {
        let uri = format!("{}object-{:03}.txt", prefix, i);
        delete_object_no_log(&uri).await?;
    }
    
    println!("✅ File LIST operations successful");
    Ok(())
}

#[tokio::test]
async fn test_file_stat_operations() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file STAT operations");
    
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
    
    println!("✅ File STAT operations successful");
    Ok(())
}

#[tokio::test]
async fn test_file_concurrent_operations() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file concurrent operations");
    
    let prefix = format!("{}concurrent-test/", base_uri);
    let num_objects = 10;
    let test_data = Bytes::from(vec![0u8; 1024]); // 1KB test data - zero-copy
    
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
    
    println!("✅ File concurrent operations successful");
    Ok(())
}

#[tokio::test]
async fn test_file_large_object() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file large object operations");
    
    let test_uri = format!("{}large-object.bin", base_uri);
    let size_mb = 10;
    let test_data = Bytes::from(vec![0xAB; size_mb * 1024 * 1024]); // 10MB (Bytes::from takes ownership)
    
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
    
    println!("✅ File large object test successful");
    Ok(())
}

#[tokio::test]
async fn test_file_nested_directories() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file nested directory operations");
    
    // Create deeply nested path (zero-copy: create as Bytes from start)
    let nested_uri = format!("{}level1/level2/level3/deep-file.txt", base_uri);
    let test_data = Bytes::from_static(b"data in nested directory");
    
    println!("  📤 PUT to nested path: {}", nested_uri);
    put_object_no_log(&nested_uri, test_data.clone()).await?;
    println!("     ✓ PUT completed");
    
    println!("  📥 GET from nested path");
    let result = get_object_no_log(&nested_uri).await?;
    assert_eq!(result, test_data);
    println!("     ✓ GET completed and verified");
    
    // List at each level
    println!("  📋 LIST at level1/");
    let level1_uri = format!("{}level1/", base_uri);
    let objects = list_objects_no_log(&level1_uri).await?;
    println!("     ✓ Found {} objects under level1/", objects.len());
    assert!(!objects.is_empty());
    
    // Cleanup
    println!("  🗑️  DELETE nested file");
    delete_object_no_log(&nested_uri).await?;
    
    println!("✅ File nested directory test successful");
    Ok(())
}

#[tokio::test]
async fn test_file_special_characters() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file paths with special characters");
    
    // Test various special characters in filenames (zero-copy: create as Bytes from start)
    let test_cases = vec![
        ("file_with_underscores.txt", Bytes::from_static(b"data with underscores")),
        ("file.multiple.dots.txt", Bytes::from_static(b"data with dots")),
        ("UPPERCASE.TXT", Bytes::from_static(b"uppercase data")),
        ("MixedCase.Txt", Bytes::from_static(b"mixed case data")),
    ];
    
    for (filename, data) in &test_cases {
        let uri = format!("{}{}", base_uri, filename);
        println!("  📤 PUT: {}", filename);
        put_object_no_log(&uri, data.clone()).await?;
        
        let result = get_object_no_log(&uri).await?;
        assert_eq!(&result, data, "Data mismatch for {}", filename);
        
        delete_object_no_log(&uri).await?;
    }
    
    println!("✅ File special characters test successful");
    Ok(())
}

#[tokio::test]
async fn test_file_empty_object() -> Result<()> {
    let (_temp_dir, base_uri) = create_test_dir()?;
    
    println!("🧪 Testing file empty object operations");
    
    let test_uri = format!("{}empty-file.txt", base_uri);
    let empty_data = Bytes::new(); // Zero-copy empty Bytes
    
    println!("  📤 PUT empty object");
    put_object_no_log(&test_uri, empty_data.clone()).await?;
    
    println!("  📥 GET empty object");
    let result = get_object_no_log(&test_uri).await?;
    assert_eq!(result.len(), 0, "Empty file should have 0 bytes");
    
    println!("  📊 STAT empty object");
    let size = stat_object_no_log(&test_uri).await?;
    assert_eq!(size, 0, "Empty file size should be 0");
    
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ File empty object test successful");
    Ok(())
}

/// Test that file:// works with absolute paths
#[tokio::test]
async fn test_file_absolute_path() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let abs_path = temp_dir.path().canonicalize()?;
    let uri = format!("file://{}/", abs_path.display());
    
    println!("🧪 Testing file absolute path: {}", uri);
    
    let test_uri = format!("{}absolute-path-test.txt", uri);
    let test_data = Bytes::from_static(b"testing absolute path");
    
    put_object_no_log(&test_uri, test_data.clone()).await?;
    let result = get_object_no_log(&test_uri).await?;
    assert_eq!(result, test_data);
    delete_object_no_log(&test_uri).await?;
    
    println!("✅ File absolute path test successful");
    Ok(())
}
