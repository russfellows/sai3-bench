//! Example demonstrating pre-flight validation usage

use sai3_bench::preflight::filesystem::validate_filesystem;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Example 1: Validate a directory for read/write workload
    println!("=== Example 1: Validating /tmp for read/write workload ===");
    let tmp_path = PathBuf::from("/tmp");
    let result = validate_filesystem(&tmp_path, true, Some(1_000_000_000)).await?;
    result.display(Some("example-agent"));

    println!("\n{}", "=".repeat(80));
    
    // Example 2: Validate a non-existent directory (will show errors)
    println!("\n=== Example 2: Validating non-existent directory ===");
    let bad_path = PathBuf::from("/this/path/does/not/exist");
    let result = validate_filesystem(&bad_path, true, None).await?;
    result.display(Some("example-agent"));
    
    println!("\n{}", "=".repeat(80));

    // Example 3: Validate for read-only workload (skips write tests)
    println!("\n=== Example 3: Validating /tmp for read-only workload ===");
    let result = validate_filesystem(&tmp_path, false, None).await?;
    result.display(Some("example-agent"));

    println!("\n{}", "=".repeat(80));
    println!("\nValidation complete!");
    println!("- Passed: {} (no errors)", result.passed());
    println!("- Errors: {}", result.error_count());
    println!("- Warnings: {}", result.warning_count());
    println!("- Info: {}", result.info_count());

    Ok(())
}
