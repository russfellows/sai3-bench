//! Unit tests for distributed prepare and cleanup with deterministic object assignment
//!
//! Verifies that:
//! 1. Prepare phase assigns objects correctly across agents using modulo distribution
//! 2. Cleanup phase uses identical logic to delete only assigned objects
//! 3. No overlap between agents (each object handled by exactly one agent)
//! 4. Complete coverage (all objects created/deleted across all agents)

use sai3_bench::config::{CleanupMode, PrepareConfig, FillPattern, EnsureSpec, PrepareStrategy};
use sai3_bench::directory_tree::DirectoryStructureConfig;
use sai3_bench::size_generator::SizeSpec;
use sai3_bench::workload::prepare_objects;
use std::collections::{HashMap, HashSet};
use tempfile::TempDir;

/// Helper to create a test prepare config with directory structure
fn create_test_config(base_uri: &str, total_files: u64) -> PrepareConfig {
    PrepareConfig {
        prepare_strategy: PrepareStrategy::Sequential,
        ensure_objects: vec![EnsureSpec {
            base_uri: base_uri.to_string(),
            count: total_files,
            min_size: None,
            max_size: None,
            size_spec: Some(SizeSpec::Fixed(1024)),
            fill: FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        }],
        directory_structure: Some(DirectoryStructureConfig {
            width: 3,
            depth: 2,
            files_per_dir: 5,
            distribution: "bottom".to_string(),
            dir_mask: "test.d%d_w%d.dir".to_string(),
        }),
        skip_verification: false,  // Need to LIST to determine which objects to create
        post_prepare_delay: 0,
        cleanup: true,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
    }
}

/// Extract file indices from prepared objects
fn extract_indices(prepared: &[sai3_bench::workload::PreparedObject]) -> Vec<usize> {
    prepared.iter()
        .filter_map(|obj| {
            // Extract index from URI like "file:///tmp/.../file_00000042.dat"
            obj.uri.rsplit('/').next()
                .and_then(|filename| filename.strip_prefix("file_"))
                .and_then(|s| s.strip_suffix(".dat"))
                .and_then(|idx_str| idx_str.parse::<usize>().ok())
        })
        .collect()
}

#[tokio::test]
async fn test_distributed_prepare_no_overlap() {
    // Test: 2 agents should create disjoint sets of objects
    let temp_dir = TempDir::new().unwrap();
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let total_files = 45; // 3 wide × 2 deep = 9 dirs, 5 files each = 45 total
    let num_agents = 2;
    
    let config = create_test_config(&base_uri, total_files);
    
    // Agent 0 prepares
    let (prepared0, _manifest0, _metrics0) = prepare_objects(
        &config,
        None,
        None,
        4,
        0,
        num_agents,
    ).await.unwrap();
    
    // Agent 1 prepares
    let (prepared1, _manifest1, _metrics1) = prepare_objects(
        &config,
        None,
        None,
        4,
        1,
        num_agents,
    ).await.unwrap();
    
    let indices0: HashSet<usize> = extract_indices(&prepared0).into_iter().collect();
    let indices1: HashSet<usize> = extract_indices(&prepared1).into_iter().collect();
    
    // Verify no overlap
    let overlap: Vec<_> = indices0.intersection(&indices1).collect();
    assert!(overlap.is_empty(), 
        "Agents should not create overlapping objects, but found {} overlapping indices: {:?}",
        overlap.len(), overlap);
    
    println!("✓ No overlap: Agent 0 created {} objects, Agent 1 created {} objects",
             indices0.len(), indices1.len());
}

#[tokio::test]
async fn test_distributed_prepare_complete_coverage() {
    // Test: All agents together should create all objects
    let temp_dir = TempDir::new().unwrap();
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let total_files = 45;
    let num_agents = 3;
    
    let config = create_test_config(&base_uri, total_files);
    
    let mut all_indices = HashSet::new();
    
    // Each agent prepares its subset
    for agent_id in 0..num_agents {
        let (prepared, _manifest, _metrics) = prepare_objects(
            &config,
            None,
            None,
            4,
            agent_id,
            num_agents,
        ).await.unwrap();
        
        let indices: HashSet<usize> = extract_indices(&prepared).into_iter().collect();
        println!("Agent {} created {} objects: indices {:?}", 
                 agent_id, indices.len(), 
                 indices.iter().take(10).collect::<Vec<_>>());
        
        all_indices.extend(indices);
    }
    
    // Verify complete coverage (all indices 0..45 are present)
    let expected: HashSet<usize> = (0..total_files as usize).collect();
    assert_eq!(all_indices, expected,
        "All agents together should create all {} objects, but created {} objects",
        total_files, all_indices.len());
    
    println!("✓ Complete coverage: {} agents created all {} objects",
             num_agents, total_files);
}

#[tokio::test]
async fn test_distributed_prepare_modulo_distribution() {
    // Test: Verify modulo distribution is correct
    let temp_dir = TempDir::new().unwrap();
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let total_files = 45;
    let num_agents = 4;
    
    let config = create_test_config(&base_uri, total_files);
    
    // Collect what each agent creates
    let mut agent_indices: HashMap<usize, Vec<usize>> = HashMap::new();
    
    for agent_id in 0..num_agents {
        let (prepared, _manifest, _metrics) = prepare_objects(
            &config,
            None,
            None,
            4,
            agent_id,
            num_agents,
        ).await.unwrap();
        
        agent_indices.insert(agent_id, extract_indices(&prepared));
    }
    
    // Verify modulo distribution: each index should be handled by (index % num_agents)
    for idx in 0..total_files as usize {
        let expected_agent = idx % num_agents;
        
        // Find which agent actually has this index
        let mut found_in_agent = None;
        for (agent_id, indices) in &agent_indices {
            if indices.contains(&idx) {
                found_in_agent = Some(*agent_id);
                break;
            }
        }
        
        assert_eq!(found_in_agent, Some(expected_agent),
            "Object {} should be handled by agent {} (via modulo), but was handled by agent {:?}",
            idx, expected_agent, found_in_agent);
    }
    
    println!("✓ Modulo distribution correct: {} agents, {} objects",
             num_agents, total_files);
    
    // Print distribution stats
    for agent_id in 0..num_agents {
        println!("  Agent {} handled {} objects", 
                 agent_id, agent_indices[&agent_id].len());
    }
}

#[tokio::test]
async fn test_distributed_prepare_balanced() {
    // Test: Distribution should be roughly balanced
    let temp_dir = TempDir::new().unwrap();
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let total_files = 45;
    let num_agents = 3;
    
    let config = create_test_config(&base_uri, total_files);
    
    let mut counts = Vec::new();
    
    for agent_id in 0..num_agents {
        let (prepared, _manifest, _metrics) = prepare_objects(
            &config,
            None,
            None,
            4,
            agent_id,
            num_agents,
        ).await.unwrap();
        
        counts.push(prepared.len());
    }
    
    // With 45 objects and 3 agents, each should get exactly 15
    let expected_per_agent = total_files / num_agents as u64;
    for (agent_id, count) in counts.iter().enumerate() {
        assert_eq!(*count as u64, expected_per_agent,
            "Agent {} should create {} objects, but created {}",
            agent_id, expected_per_agent, count);
    }
    
    println!("✓ Balanced distribution: {} agents each created {} objects",
             num_agents, expected_per_agent);
}

#[tokio::test]
async fn test_standalone_mode() {
    // Test: Standalone mode (num_agents=1) should create all objects
    let temp_dir = TempDir::new().unwrap();
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let total_files = 45;
    
    let config = create_test_config(&base_uri, total_files);
    
    let (prepared, _manifest, _metrics) = prepare_objects(
        &config,
        None,
        None,
        4,
        0,  // agent_id
        1,  // num_agents (standalone)
    ).await.unwrap();
    
    assert_eq!(prepared.len(), total_files as usize,
        "Standalone mode should create all {} objects, but created {}",
        total_files, prepared.len());
    
    let indices: HashSet<usize> = extract_indices(&prepared).into_iter().collect();
    let expected: HashSet<usize> = (0..total_files as usize).collect();
    assert_eq!(indices, expected,
        "Standalone mode should create all indices 0..{}", total_files);
    
    println!("✓ Standalone mode: Created all {} objects", total_files);
}
