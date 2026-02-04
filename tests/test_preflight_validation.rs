//! Tests for pre-flight filesystem validation

use sai3_bench::preflight::filesystem::validate_filesystem;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn test_validate_nonexistent_directory() {
    let nonexistent = PathBuf::from("/tmp/sai3bench_nonexistent_test_12345");
    
    let result = validate_filesystem(&nonexistent, false, None).await.unwrap();
    
    // Should have error about directory not existing
    assert!(result.has_errors());
    let errors: Vec<_> = result.results.iter()
        .filter(|r| r.level == sai3_bench::preflight::ResultLevel::Error)
        .collect();
    assert!(!errors.is_empty());
    assert!(errors[0].message.contains("does not exist"));
}

#[tokio::test]
async fn test_validate_existing_writable_directory() {
    let temp_dir = TempDir::new().unwrap();
    
    let result = validate_filesystem(temp_dir.path(), true, None).await.unwrap();
    
    // Should pass validation
    assert!(!result.has_errors(), "Validation should pass for writable temp directory");
    
    // Should have info about user identity
    let info_results: Vec<_> = result.results.iter()
        .filter(|r| r.level == sai3_bench::preflight::ResultLevel::Info)
        .collect();
    assert!(!info_results.is_empty(), "Should have info about user identity");
    
    // Should have success for stat, list, write, mkdir, delete
    let success_results: Vec<_> = result.results.iter()
        .filter(|r| r.level == sai3_bench::preflight::ResultLevel::Success)
        .collect();
    assert!(!success_results.is_empty(), "Should have success results");
}

#[tokio::test]
async fn test_validate_readonly_directory() {
    let temp_dir = TempDir::new().unwrap();
    let test_dir = temp_dir.path().join("readonly");
    fs::create_dir(&test_dir).unwrap();
    
    // Make directory read-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&test_dir).unwrap().permissions();
        perms.set_mode(0o555); // r-xr-xr-x
        fs::set_permissions(&test_dir, perms).unwrap();
    }
    
    let result = validate_filesystem(&test_dir, true, None).await.unwrap();
    
    // Should have error about write permission
    assert!(result.has_errors(), "Should detect write permission error");
    
    let write_errors: Vec<_> = result.results.iter()
        .filter(|r| r.test_phase == "write" && r.level == sai3_bench::preflight::ResultLevel::Error)
        .collect();
    assert!(!write_errors.is_empty(), "Should have write permission error");
    
    // Clean up - restore permissions so temp_dir can be deleted
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&test_dir).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&test_dir, perms).unwrap();
    }
}

#[tokio::test]
async fn test_validate_insufficient_disk_space() {
    let temp_dir = TempDir::new().unwrap();
    
    // Request more space than exists on any system (1 petabyte)
    let required_space = 1_000_000_000_000_000u64;
    
    let result = validate_filesystem(temp_dir.path(), true, Some(required_space)).await.unwrap();
    
    // Should have error about insufficient disk space
    let disk_errors: Vec<_> = result.results.iter()
        .filter(|r| r.test_phase == "disk-space" && r.level == sai3_bench::preflight::ResultLevel::Error)
        .collect();
    assert!(!disk_errors.is_empty(), "Should detect insufficient disk space");
}

#[tokio::test]
async fn test_validate_display_output() {
    let temp_dir = TempDir::new().unwrap();
    
    let result = validate_filesystem(temp_dir.path(), true, None).await.unwrap();
    
    // Test display function doesn't panic
    result.display(Some("test-agent"));
    result.display(None);
}

#[tokio::test]
async fn test_progressive_testing_stops_on_error() {
    let nonexistent = PathBuf::from("/tmp/sai3bench_nonexistent_test_67890");
    
    let result = validate_filesystem(&nonexistent, true, None).await.unwrap();
    
    // Should stop after stat test fails
    // Should NOT have write/mkdir/delete results since stat failed
    let test_phases: Vec<_> = result.results.iter()
        .map(|r| r.test_phase.as_str())
        .collect();
    
    // Should have identity and stat, but not later stages
    assert!(test_phases.contains(&"identity"));
    assert!(test_phases.contains(&"stat"));
    assert!(!test_phases.contains(&"write"), "Should not test write after stat fails");
    assert!(!test_phases.contains(&"delete"), "Should not test delete after stat fails");
}
