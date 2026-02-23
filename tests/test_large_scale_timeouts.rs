// tests/test_large_scale_timeouts.rs
//
// Unit tests for large-scale scenarios (300k+ directories, 64M+ files)
// Validates timeout configuration, barrier sync settings, and config parsing
// for operations that take minutes to hours.
//
// NOTE: These tests focus on configuration validation and timeout calculations,
// NOT actual data generation (which would be too slow for unit tests).

use sai3_bench::config::{BarrierSyncConfig, DistributedConfig, PrepareConfig, TreeCreationMode};
use sai3_bench::directory_tree::DirectoryStructureConfig;

// ============================================================================
// Test: Tree Structure Size Calculations
// ============================================================================

#[test]
fn test_calculate_tree_sizes() {
    // Test that we can calculate expected tree sizes correctly
    // without actually generating them
    
    struct TreeSize {
        width: usize,
        depth: usize,
        files_per_dir: usize,
        expected_dirs: u64,
        expected_files: u64,
    }
    
    let tests = vec![
        TreeSize {
            width: 24,
            depth: 2,
            files_per_dir: 193,
            expected_dirs: 576,         // 24^2
            expected_files: 111_168,    // 576 * 193
        },
        TreeSize {
            width: 24,
            depth: 3,
            files_per_dir: 193,
            expected_dirs: 13_824,      // 24^3
            expected_files: 2_668_032,  // 13824 * 193
        },
        TreeSize {
            width: 24,
            depth: 4,
            files_per_dir: 193,
            expected_dirs: 331_776,     // 24^4
            expected_files: 64_032_768, // 331776 * 193
        },
    ];
    
    for test in tests {
        let total_dirs = (test.width as u64).pow(test.depth as u32);
        let total_files = total_dirs * test.files_per_dir as u64;
        
        assert_eq!(
            total_dirs, test.expected_dirs,
            "width={}, depth={} should have {} dirs",
            test.width, test.depth, test.expected_dirs
        );
        
        assert_eq!(
            total_files, test.expected_files,
            "width={}, depth={} should have {} files",
            test.width, test.depth, test.expected_files
        );
        
        println!(
            "✓ Tree {}^{} = {} dirs × {} files/dir = {} total files",
            test.width, test.depth, total_dirs, test.files_per_dir, total_files
        );
    }
}

// ============================================================================
// Test: Barrier Timeout Configuration
// ============================================================================

#[test]
fn test_barrier_sync_timeout_config() {
    // Verify that barrier sync can be configured with longer timeouts
    // for large-scale operations
    
    let yaml = r#"
enabled: true
default_heartbeat_interval: 60  # 60s between heartbeats
default_missed_threshold: 5     # Allow 5 missed (5 * 60s = 300s total)
default_query_timeout: 30       # 30s to query agent status
default_query_retries: 3        # 3 retries (3 * 30s = 90s total)

# Override prepare phase to be more lenient (can take minutes)
prepare:
  type: all_or_nothing
  heartbeat_interval: 120       # 2 minutes between heartbeats
  missed_threshold: 10          # Allow 10 missed (20 minutes total)
  query_timeout: 60             # 1 minute per query
  query_retries: 5              # 5 retries (5 minutes total query time)
"#;
    
    let config: BarrierSyncConfig = serde_yaml::from_str(yaml)
        .expect("Should parse barrier sync config");
    
    assert!(config.enabled);
    assert_eq!(config.default_heartbeat_interval, 60);
    assert_eq!(config.default_missed_threshold, 5);
    assert_eq!(config.default_query_timeout, 30);
    assert_eq!(config.default_query_retries, 3);
    
    // Verify prepare phase overrides
    let prepare_config = config.get_phase_config("prepare");
    assert_eq!(prepare_config.heartbeat_interval, 120);
    assert_eq!(prepare_config.missed_threshold, 10);
    assert_eq!(prepare_config.query_timeout, 60);
    assert_eq!(prepare_config.query_retries, 5);
    
    // Calculate effective timeout for prepare phase
    // Total timeout = heartbeat_interval * missed_threshold + query_timeout * query_retries
    let heartbeat_timeout = prepare_config.heartbeat_interval as u64 * prepare_config.missed_threshold as u64;
    let query_timeout = prepare_config.query_timeout * prepare_config.query_retries as u64;
    let total_timeout = heartbeat_timeout + query_timeout;
    
    println!(
        "✓ Prepare phase timeout: {}s heartbeat + {}s query = {}s total ({:.1} min)",
        heartbeat_timeout,
        query_timeout,
        total_timeout,
        total_timeout as f64 / 60.0
    );
    
    // For 331k tree generation (~90s), this should be sufficient
    assert!(
        total_timeout >= 90,
        "Total timeout {}s should be >= 90s for large tree generation",
        total_timeout
    );
}

#[test]
fn test_barrier_sync_default_timeout_insufficient() {
    // Document that DEFAULT barrier config is insufficient for large-scale operations
    // This test PASSES to show the problem exists
    
    let config = BarrierSyncConfig::default();
    
    let prepare_config = config.get_phase_config("prepare");
    
    // Default: 30s heartbeat * 3 missed = 90s total
    let heartbeat_timeout = prepare_config.heartbeat_interval * prepare_config.missed_threshold as u64;
    let query_timeout = prepare_config.query_timeout * prepare_config.query_retries as u64;
    let total_timeout = heartbeat_timeout + query_timeout;
    
    println!(
        "⚠ DEFAULT prepare timeout: {}s heartbeat + {}s query = {}s total",
        heartbeat_timeout,
        query_timeout,
        total_timeout
    );
    
    // For 331k tree generation (~90s), default config is BARELY sufficient
    // If tree generation takes 91s, it will timeout!
    assert_eq!(total_timeout, 110, "Default total timeout is 110s");
    
    println!("⚠ WARNING: Default 110s timeout may be insufficient for 331k tree (can take 90s+)");
    println!("   Recommendation: Use custom barrier_sync config with longer timeouts");
}

// ============================================================================
// Test: Large-Scale Directory Structure Configuration
// ============================================================================

#[test]
fn test_directory_structure_config_parsing() {
    // Test parsing of the 4-host test config directory structure
    
    let yaml = r#"
width: 24
depth: 4
files_per_dir: 193
distribution: "bottom"
dir_mask: "d%d_w%d.dir"
"#;
    
    let dir_struct: DirectoryStructureConfig = serde_yaml::from_str(yaml)
        .expect("Should parse directory structure");
    
    assert_eq!(dir_struct.width, 24);
    assert_eq!(dir_struct.depth, 4);
    assert_eq!(dir_struct.files_per_dir, 193);
    assert_eq!(dir_struct.distribution, "bottom");
    assert_eq!(dir_struct.dir_mask, "d%d_w%d.dir".to_string());
    
    // Calculate expected totals
    let total_dirs = 24_u64.pow(4); // 331,776
    let total_files = total_dirs * 193; // 64,032,768
    
    assert_eq!(total_dirs, 331776);
    assert_eq!(total_files, 64032768);
    
    println!("✓ Parsed large-scale directory structure config:");
    println!("  {} directories ({} levels deep)", total_dirs, 4);
    println!("  {} files total ({} per directory)", total_files, 193);
}

// ============================================================================
// Test: Prepare Phase Timeout Configuration
// ============================================================================

#[test]
fn test_prepare_config_with_large_object_count() {
    // Validate that PrepareConfig can handle 64M objects
    
    let yaml = r#"
prepare_strategy: parallel
skip_verification: false
force_overwrite: true

directory_structure:
  width: 24
  depth: 4
  files_per_dir: 193
  distribution: "bottom"
  dir_mask: "d%d_w%d.dir"

ensure_objects:
  - count: 64032768
    fill: random
    size_distribution:
      type: lognormal
      mean: 8MiB
      std_dev: 1MiB
      min: 1MiB
      max: 16MiB
    use_multi_endpoint: true

cleanup: false
"#;
    
    let config: PrepareConfig = serde_yaml::from_str(yaml)
        .expect("Should parse prepare config with 64M objects");
    
    assert_eq!(config.ensure_objects.len(), 1);
    assert_eq!(config.ensure_objects[0].count, 64032768);
    assert!(!config.skip_verification);
    assert!(config.force_overwrite);
    
    // Verify directory structure
    let dir_struct = config.directory_structure.as_ref()
        .expect("Should have directory structure");
    assert_eq!(dir_struct.width, 24);
    assert_eq!(dir_struct.depth, 4);
    assert_eq!(dir_struct.files_per_dir, 193);
    
    println!("✓ Parsed prepare config for 64M objects");
    println!("  Directory structure: {}^{} = {} dirs", 24, 4, 331776);
    println!("  Files per directory: {}", 193);
    println!("  Total files: {}", 64032768);
}

// ============================================================================
// Test: Multi-Endpoint Agent Configuration
// ============================================================================

#[test]
fn test_distributed_config_4_agents_16_endpoints() {
    // Test the configuration for 4 agents with 4 endpoints each (16 total)
    
    let yaml = r#"
shared_filesystem: false
tree_creation_mode: isolated
path_selection: random

agents:
  - address: "172.21.4.10:7761"
    id: "agent-1"
    concurrency_override: 16
    multi_endpoint:
      endpoints:
        - "file:///mnt/filesys1/benchmark/"
        - "file:///mnt/filesys2/benchmark/"
        - "file:///mnt/filesys3/benchmark/"
        - "file:///mnt/filesys4/benchmark/"
      strategy: least_connections

  - address: "172.21.4.11:7761"
    id: "agent-2"
    concurrency_override: 16
    multi_endpoint:
      endpoints:
        - "file:///mnt/filesys5/benchmark/"
        - "file:///mnt/filesys6/benchmark/"
        - "file:///mnt/filesys7/benchmark/"
        - "file:///mnt/filesys8/benchmark/"
      strategy: least_connections

  - address: "172.21.4.12:7761"
    id: "agent-3"
    concurrency_override: 16
    multi_endpoint:
      endpoints:
        - "file:///mnt/filesys9/benchmark/"
        - "file:///mnt/filesys10/benchmark/"
        - "file:///mnt/filesys11/benchmark/"
        - "file:///mnt/filesys12/benchmark/"
      strategy: least_connections

  - address: "172.21.4.13:7761"
    id: "agent-4"
    concurrency_override: 16
    multi_endpoint:
      endpoints:
        - "file:///mnt/filesys13/benchmark/"
        - "file:///mnt/filesys14/benchmark/"
        - "file:///mnt/filesys15/benchmark/"
        - "file:///mnt/filesys16/benchmark/"
      strategy: least_connections
"#;
    
    let config: DistributedConfig = serde_yaml::from_str(yaml)
        .expect("Should parse distributed config");
    
    assert_eq!(config.agents.len(), 4, "Should have 4 agents");
    assert!(!config.shared_filesystem);
    assert_eq!(config.tree_creation_mode, TreeCreationMode::Isolated);
    
    // Verify each agent has 4 endpoints
    for (i, agent) in config.agents.iter().enumerate() {
        assert_eq!(
            agent.id,
            Some(format!("agent-{}", i + 1)),
            "Agent {} should have correct ID",
            i + 1
        );
        
        let endpoints = agent.multi_endpoint.as_ref()
            .expect("Agent should have multi_endpoint config");
        
        assert_eq!(
            endpoints.endpoints.len(),
            4,
            "Agent {} should have 4 endpoints",
            i + 1
        );
        
        assert_eq!(
            agent.concurrency_override,
            Some(16),
            "Agent {} should have concurrency override of 16",
            i + 1
        );
    }
    
    // Calculate total endpoint coverage
    let total_endpoints: usize = config.agents.iter()
        .filter_map(|a| a.multi_endpoint.as_ref())
        .map(|m| m.endpoints.len())
        .sum();
    
    assert_eq!(total_endpoints, 16, "Should have 16 total endpoints");
    
    println!("✓ 4-agent distributed config validated:");
    println!("  {} agents × 4 endpoints = {} total mount points", 4, total_endpoints);
    println!("  Concurrency per agent: 16");
    println!("  Total concurrency: {}", 4 * 16);
}

// ============================================================================
// Test: Progress Reporting Intervals
// ============================================================================

#[test]
fn test_progress_reporting_prevents_timeout() {
    // Verify that progress reporting intervals are sufficient to prevent timeouts
    // during long-running operations
    
    use sai3_bench::constants::{
        LISTING_PROGRESS_INTERVAL,
        DEFAULT_AGENT_HEARTBEAT_SECS,
    };
    
    // Listing progress updates every 1000 files
    assert_eq!(LISTING_PROGRESS_INTERVAL, 1000);
    
    // With 64M files, how often do we get updates?
    let total_files = 64_032_768_u64;
    let num_updates = total_files / LISTING_PROGRESS_INTERVAL;
    
    println!("✓ Listing progress for 64M files:");
    println!("  {} updates (every {} files)", num_updates, LISTING_PROGRESS_INTERVAL);
    
    // At 10k files/sec listing speed, how often are updates?
    let listing_rate = 10_000.0; // files/sec (conservative estimate)
    let update_interval_secs = LISTING_PROGRESS_INTERVAL as f64 / listing_rate;
    
    println!("  Update every {:.2}s @ {}k files/sec", update_interval_secs, listing_rate / 1000.0);
    
    // Heartbeat is every 1 second by default
    println!("  Agent heartbeat: every {}s", DEFAULT_AGENT_HEARTBEAT_SECS);
    
    // Verify that updates are frequent enough to prevent timeout
    // Default barrier config: 30s heartbeat * 3 missed = 90s timeout
    assert!(
        update_interval_secs < 30.0,
        "Update interval ({:.2}s) should be < 30s heartbeat interval",
        update_interval_secs
    );
}

// ============================================================================
// Test: Timeout Calculation for Large-Scale Operations
// ============================================================================

#[test]
fn test_calculate_required_timeouts_for_scale() {
    // Calculate minimum timeout requirements for different scales
    
    struct ScaleTest {
        name: &'static str,
        #[allow(dead_code)]  // Test metadata for documentation
        width: usize,
        #[allow(dead_code)]  // Test metadata for documentation
        depth: usize,
        #[allow(dead_code)]  // Test metadata for documentation
        total_dirs: u64,
        #[allow(dead_code)]  // Test metadata for documentation
        total_files: u64,
        tree_gen_time_secs: u64,
        listing_time_secs: u64,
        create_time_secs: u64,
    }
    
    let tests = vec![
        ScaleTest {
            name: "Simple (576 dirs, 111k files)",
            width: 24,
            depth: 2,
            total_dirs: 576,
            total_files: 111_168,
            tree_gen_time_secs: 1,     // ~1 second tree generation
            listing_time_secs: 11,      // ~10k files/sec = 11s
            create_time_secs: 111,      // ~1k creates/sec = 111s
        },
        ScaleTest {
            name: "Medium (13.8k dirs, 2.7M files)",
            width: 24,
            depth: 3,
            total_dirs: 13_824,
            total_files: 2_668_032,
            tree_gen_time_secs: 5,      // ~5 seconds tree generation
            listing_time_secs: 267,     // ~10k files/sec = 267s
            create_time_secs: 2668,     // ~1k creates/sec = 2668s (~45 min)
        },
        ScaleTest {
            name: "Large (331k dirs, 64M files)",
            width: 24,
            depth: 4,
            total_dirs: 331_776,
            total_files: 64_032_768,
            tree_gen_time_secs: 90,     // ~90 seconds tree generation
            listing_time_secs: 6403,    // ~10k files/sec = 6403s (~107 min)
            create_time_secs: 64033,    // ~1k creates/sec = 64033s (~17.8 hours)
        },
    ];
    
    println!("\n=== Timeout Requirements Analysis ===\n");
    
    for test in &tests {
        println!("{}:", test.name);
        println!("  Tree generation: {}s ({:.1} min)", test.tree_gen_time_secs, test.tree_gen_time_secs as f64 / 60.0);
        println!("  Listing phase: {}s ({:.1} min)", test.listing_time_secs, test.listing_time_secs as f64 / 60.0);
        println!("  Create phase: {}s ({:.1} hours)", test.create_time_secs, test.create_time_secs as f64 / 3600.0);
        
        // Recommend barrier timeout (2x theoretical time for safety margin)
        let recommended_timeout = test.tree_gen_time_secs * 2;
        println!("  Recommended tree timeout: {}s (2x safety margin)", recommended_timeout);
        
        let recommended_listing_timeout = test.listing_time_secs * 2;
        println!("  Recommended listing timeout: {}s ({:.1} hours)", recommended_listing_timeout, recommended_listing_timeout as f64 / 3600.0);
        
        println!();
    }
    
    // This test always passes - it's for documentation/analysis
    assert!(true, "Timeout analysis complete");
}
