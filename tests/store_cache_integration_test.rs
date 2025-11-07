// tests/store_cache_integration_test.rs
// Integration test to verify ObjectStore caching works correctly

use tempfile::TempDir;
use std::process::Command;

#[test]
fn test_store_cache_with_file_backend() {
    // Create temp directory for test data
    let temp_dir = TempDir::new().unwrap();
    let test_dir = temp_dir.path().join("sai3-store-cache-test");
    std::fs::create_dir_all(&test_dir).unwrap();
    
    // Run workload with store caching enabled (default)
    let output = Command::new(env!("CARGO_BIN_EXE_sai3-bench"))
        .arg("run")
        .arg("--config")
        .arg("tests/configs/store_cache_test.yaml")
        .output()
        .expect("Failed to execute command");
    
    // Verify command succeeded
    assert!(
        output.status.success(),
        "sai3-bench run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    
    // Verify output contains expected metrics
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PUT"), "Output should contain PUT operations");
    assert!(stdout.contains("ops/s"), "Output should contain throughput metrics");
}

#[test]
fn test_config_parsing_dry_run() {
    // Test dry-run functionality
    let output = Command::new(env!("CARGO_BIN_EXE_sai3-bench"))
        .arg("run")
        .arg("--config")
        .arg("tests/configs/store_cache_test.yaml")
        .arg("--dry-run")
        .output()
        .expect("Failed to execute command");
    
    assert!(output.status.success(), "Dry-run failed: {}", 
            String::from_utf8_lossy(&output.stderr));
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // Verify config is parsed correctly and dry-run output is formatted properly
    assert!(stdout.contains("Target URI:"), 
           "Dry-run should contain 'Target URI:' line, got:\n{}", stdout);
    assert!(stdout.contains("file:///tmp/sai3-store-cache-test/"),
           "Dry-run should show the test directory path, got:\n{}", stdout);
    assert!(stdout.contains("Configuration is valid"),
           "Dry-run should confirm config validation, got:\n{}", stdout);
}

#[test] 
fn test_basic_file_operations() {
    // Simple PUT test without complex config options
    let yaml = r#"
target: "file:///tmp/sai3-test-basic"
duration: "1s"
concurrency: 1

workload:
  - op: put
    path: "data/"
    object_size: 512
    weight: 100
"#;
    
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("test_config.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    
    // Ensure test directory exists
    std::fs::create_dir_all("/tmp/sai3-test-basic/data").unwrap();
    
    // Run the workload
    let output = Command::new(env!("CARGO_BIN_EXE_sai3-bench"))
        .arg("run")
        .arg("--config")
        .arg(&config_path)
        .output()
        .expect("Failed to execute command");
    
    assert!(output.status.success(), "Basic workload failed: {}",
           String::from_utf8_lossy(&output.stderr));
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show PUT operations completed
    assert!(stdout.contains("PUT") || stdout.contains("WRITE"),
           "Output should show PUT/WRITE operations");
}

#[test]
fn test_process_scaling_config_dry_run() {
    // Verify process scaling configuration is parsed and displayed correctly
    let output = Command::new(env!("CARGO_BIN_EXE_sai3-bench"))
        .arg("run")
        .arg("--config")
        .arg("tests/configs/process_scaling_test.yaml")
        .arg("--dry-run")
        .output()
        .expect("Failed to execute command");
    
    assert!(output.status.success(), "Process scaling dry-run failed: {}", 
            String::from_utf8_lossy(&output.stderr));
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // Verify process scaling is shown in dry-run output
    assert!(stdout.contains("processes") || stdout.contains("Processes:") || stdout.contains("auto"),
           "Dry-run should show process scaling configuration, got:\n{}", stdout);
    assert!(stdout.contains("Configuration is valid"),
           "Dry-run should confirm config validation, got:\n{}", stdout);
}
