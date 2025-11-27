//! Simplified test to debug distributed prepare

use sai3_bench::config::{CleanupMode, PrepareConfig, FillPattern, EnsureSpec, PrepareStrategy};
use sai3_bench::size_generator::SizeSpec;
use sai3_bench::workload::prepare_objects;
use tempfile::TempDir;

#[tokio::test]
async fn test_single_agent_prepare() {
    println!("Test starting...");
    println!("Creating temp dir...");
    let temp_dir = TempDir::new().unwrap();
    println!("Temp dir created");
    let base_uri = format!("file://{}", temp_dir.path().display());
    println!("Temp dir: {}", base_uri);
    
    println!("Creating config...");
    let config = PrepareConfig {
        prepare_strategy: PrepareStrategy::Sequential,
        ensure_objects: vec![EnsureSpec {
            base_uri: base_uri.clone(),
            count: 10,  // Just 10 objects
            min_size: None,
            max_size: None,
            size_spec: Some(SizeSpec::Fixed(1024)),
            fill: FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        }],
        directory_structure: None,  // No directory structure - flat files
        skip_verification: false,  // Enable LIST to test distributed prepare
        post_prepare_delay: 0,
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
    };
    println!("Config created");
    
    println!("About to call prepare_objects for agent 0/2...");
    let result = prepare_objects(
        &config,
        None,
        None,
        2,  // concurrency
        0,  // agent_id
        2,  // num_agents
    ).await;
    println!("prepare_objects returned!");
    
    let (prepared, _manifest, _metrics) = result.unwrap();
    
    println!("Agent 0 prepared {} objects", prepared.len());
    assert_eq!(prepared.len(), 5, "Agent 0 should create 5 objects (indices 0,2,4,6,8)");
}
