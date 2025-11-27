// tests/test_distributed_cleanup.rs
//! Integration tests for distributed cleanup functionality
//!
//! Tests the deterministic file assignment logic that ensures each agent
//! cleans up exactly its assigned subset of objects without overlap or gaps.

use anyhow::Result;
use sai3_bench::directory_tree::{DirectoryStructureConfig, DirectoryTree, TreeManifest};
use std::collections::HashSet;

#[test]
fn test_agent_file_assignment_no_overlap() -> Result<()> {
    // Create a tree with known structure
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 10,
        distribution: "all".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    let num_agents = 3;
    manifest.assign_agents(num_agents);
    
    // Collect all file indices assigned to all agents
    let mut all_assigned_files = HashSet::new();
    let mut agent_assignments = Vec::new();
    
    for agent_id in 0..num_agents {
        let indices = manifest.get_agent_file_indices(agent_id, num_agents);
        agent_assignments.push(indices.clone());
        
        for idx in indices {
            // Verify no overlap: each file should appear exactly once
            assert!(!all_assigned_files.contains(&idx), 
                "File index {} assigned to multiple agents", idx);
            all_assigned_files.insert(idx);
        }
    }
    
    // Verify complete coverage: all files should be assigned
    assert_eq!(all_assigned_files.len(), manifest.total_files,
        "Not all files were assigned to agents");
    
    // Verify contiguous coverage
    for idx in 0..manifest.total_files {
        assert!(all_assigned_files.contains(&idx),
            "File index {} was not assigned to any agent", idx);
    }
    
    println!("✓ No overlap test passed: {} files distributed across {} agents",
        manifest.total_files, num_agents);
    println!("  Agent 0: {} files", agent_assignments[0].len());
    println!("  Agent 1: {} files", agent_assignments[1].len());
    println!("  Agent 2: {} files", agent_assignments[2].len());
    
    Ok(())
}

#[test]
fn test_agent_file_assignment_modulo_distribution() -> Result<()> {
    // Verify that assignment follows modulo logic: file_idx % num_agents == agent_id
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 10,
        distribution: "all".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    let num_agents = 4;
    manifest.assign_agents(num_agents);
    
    for agent_id in 0..num_agents {
        let indices = manifest.get_agent_file_indices(agent_id, num_agents);
        
        // Verify each assigned index follows modulo rule
        for idx in indices {
            assert_eq!(idx % num_agents, agent_id,
                "File index {} assigned to agent {} but {} % {} = {}",
                idx, agent_id, idx, num_agents, idx % num_agents);
        }
    }
    
    println!("✓ Modulo distribution test passed");
    Ok(())
}

#[test]
fn test_single_agent_gets_all_files() -> Result<()> {
    // When num_agents == 1, single agent should get all files
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 10,
        distribution: "all".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    manifest.assign_agents(1);
    
    let indices = manifest.get_agent_file_indices(0, 1);
    assert_eq!(indices.len(), manifest.total_files,
        "Single agent should get all files");
    
    println!("✓ Single agent test passed: 1 agent assigned {} files",
        manifest.total_files);
    Ok(())
}

#[test]
fn test_should_agent_handle_file_logic() -> Result<()> {
    // Test the should_agent_handle_file predicate
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 10,
        distribution: "bottom".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    let num_agents = 3;
    manifest.assign_agents(num_agents);
    
    // Test specific file indices
    assert!(manifest.should_agent_handle_file(0, 0, 3));   // 0 % 3 == 0
    assert!(manifest.should_agent_handle_file(1, 1, 3));   // 1 % 3 == 1
    assert!(manifest.should_agent_handle_file(2, 2, 3));   // 2 % 3 == 2
    assert!(manifest.should_agent_handle_file(3, 0, 3));   // 3 % 3 == 0
    
    assert!(!manifest.should_agent_handle_file(0, 1, 3));  // 0 % 3 != 1
    assert!(!manifest.should_agent_handle_file(1, 2, 3));  // 1 % 3 != 2
    
    println!("✓ should_agent_handle_file logic test passed");
    Ok(())
}

#[test]
fn test_get_agent_file_paths() -> Result<()> {
    // Test that get_agent_file_paths returns valid full paths
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 5,
        distribution: "bottom".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    let num_agents = 2;
    manifest.assign_agents(num_agents);
    
    let agent0_paths = manifest.get_agent_file_paths(0, num_agents);
    let agent1_paths = manifest.get_agent_file_paths(1, num_agents);
    
    println!("Agent 0 paths ({}): {:?}", agent0_paths.len(), &agent0_paths[..agent0_paths.len().min(3)]);
    println!("Agent 1 paths ({}): {:?}", agent1_paths.len(), &agent1_paths[..agent1_paths.len().min(3)]);
    
    // Verify paths have correct format
    for path in &agent0_paths {
        assert!(path.contains('/'), "Path should contain directory separator: {}", path);
        assert!(path.ends_with(".dat"), "Path should end with .dat: {}", path);
    }
    
    // Verify no overlap
    let set0: HashSet<_> = agent0_paths.into_iter().collect();
    let set1: HashSet<_> = agent1_paths.into_iter().collect();
    
    let intersection: Vec<_> = set0.intersection(&set1).collect();
    assert!(intersection.is_empty(), 
        "Agents should not have overlapping paths: {:?}", intersection);
    
    println!("✓ get_agent_file_paths test passed");
    Ok(())
}

#[test]
fn test_balanced_distribution() -> Result<()> {
    // Verify that files are distributed relatively evenly across agents
    let config = DirectoryStructureConfig {
        width: 3,
        depth: 2,
        files_per_dir: 10,
        distribution: "all".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    let num_agents = 5;
    manifest.assign_agents(num_agents);
    
    let mut counts = Vec::new();
    for agent_id in 0..num_agents {
        let count = manifest.get_agent_file_indices(agent_id, num_agents).len();
        counts.push(count);
    }
    
    // Calculate distribution statistics
    let min_count = *counts.iter().min().unwrap();
    let max_count = *counts.iter().max().unwrap();
    let total: usize = counts.iter().sum();
    let avg = total as f64 / num_agents as f64;
    
    println!("Distribution across {} agents:", num_agents);
    for (i, count) in counts.iter().enumerate() {
        println!("  Agent {}: {} files ({:.1}%)", i, count, (*count as f64 / total as f64) * 100.0);
    }
    println!("  Min: {}, Max: {}, Avg: {:.1}", min_count, max_count, avg);
    
    // With modulo distribution, difference should be at most 1
    assert!(max_count - min_count <= 1,
        "Distribution imbalance too large: max={}, min={}", max_count, min_count);
    
    println!("✓ Balanced distribution test passed");
    Ok(())
}

#[test]
fn test_cleanup_mode_enum() {
    // Verify CleanupMode enum has expected values
    use sai3_bench::config::CleanupMode;
    
    let strict = CleanupMode::Strict;
    let tolerant = CleanupMode::Tolerant;
    let best_effort = CleanupMode::BestEffort;
    
    // Verify default is tolerant
    assert_eq!(CleanupMode::default(), CleanupMode::Tolerant);
    
    // Verify they're different
    assert_ne!(strict, tolerant);
    assert_ne!(tolerant, best_effort);
    assert_ne!(strict, best_effort);
    
    println!("✓ CleanupMode enum test passed");
}

#[test]
fn test_large_scale_distribution() -> Result<()> {
    // Test with larger numbers to ensure scalability
    let config = DirectoryStructureConfig {
        width: 5,
        depth: 3,
        files_per_dir: 100,
        distribution: "bottom".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config)?;
    let mut manifest = TreeManifest::from_tree(&tree);
    
    let num_agents = 8;
    manifest.assign_agents(num_agents);
    
    println!("Large scale test: {} total files across {} agents",
        manifest.total_files, num_agents);
    
    // Verify no overlap and complete coverage
    let mut all_files = HashSet::new();
    for agent_id in 0..num_agents {
        let indices = manifest.get_agent_file_indices(agent_id, num_agents);
        for idx in indices {
            assert!(!all_files.contains(&idx), 
                "Overlap detected at file index {}", idx);
            all_files.insert(idx);
        }
    }
    
    assert_eq!(all_files.len(), manifest.total_files,
        "Coverage incomplete: {} assigned vs {} total",
        all_files.len(), manifest.total_files);
    
    println!("✓ Large scale distribution test passed");
    Ok(())
}
