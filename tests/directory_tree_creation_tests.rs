// tests/directory_tree_creation_tests.rs
//! Integration tests for directory tree creation with multi-agent coordination
//! 
//! CRITICAL: These tests verify we avoid the rdf-bench collision bug (v2025.10.0)
//! where multiple workers created files with identical names on shared filesystems

use anyhow::Result;
use sai3_bench::config::{PrepareConfig, CleanupMode};
use sai3_bench::directory_tree::DirectoryStructureConfig;
use sai3_bench::prepare::{create_directory_tree, PrepareMetrics};
use std::collections::HashSet;
use tempfile::TempDir;

/// Test single agent creates all directories and files
#[tokio::test]
async fn test_single_agent_creates_all() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 3,  // Fixed: was 6, but test expects 12 files (4 leaf dirs * 3)
            distribution: "bottom".to_string(),
            dir_mask: "dir_%d_w%d".to_string(),
        }),
        prepare_strategy: Default::default(),
        skip_verification: false,
    };
    
    let mut metrics = PrepareMetrics::default();
    let manifest = create_directory_tree(&config, 0, 1, &base_uri, &mut metrics, None).await?;
    
    // Verify directory count: 2 + 4 = 6 directories
    assert_eq!(manifest.total_dirs, 6);
    
    // Verify file count: 4 leaf dirs * 3 files = 12 files
    assert_eq!(manifest.total_files, 12);
    
    // Verify all directories were created
    let created_dirs = list_directories(temp_dir.path())?;
    assert_eq!(created_dirs.len(), 6, "Should have created 6 directories");
    
    // Verify all files were created (only in leaf directories for "bottom" distribution)
    let created_files = list_files_recursive(temp_dir.path())?;
    assert_eq!(created_files.len(), 12, "Should have created 12 files");
    
    Ok(())
}

/// Test multiple agents create non-overlapping files (CRITICAL: rdf-bench bug test)
#[tokio::test]
async fn test_multi_agent_no_collision() -> Result<()> {
    let config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 10,  // 40 total files
            distribution: "bottom".to_string(),
            dir_mask: "test.d%d_w%d.dir".to_string(),
        }),
        prepare_strategy: Default::default(),
        skip_verification: false,
    };
    
    let num_agents = 4;
    let mut all_files = HashSet::new();
    
    // Simulate 4 agents creating files
    for agent_id in 0..num_agents {
        let temp_agent_dir = TempDir::new()?;
        let agent_base_uri = format!("file://{}", temp_agent_dir.path().display());
        
        let mut metrics = PrepareMetrics::default();
        let manifest = create_directory_tree(&config, agent_id, num_agents, &agent_base_uri, &mut metrics, None).await?;
        
        // Total files should be 4 leaf dirs * 10 files = 40
        assert_eq!(manifest.total_files, 40);
        
        // Collect file names created by this agent
        let created_files = list_files_recursive(temp_agent_dir.path())?;
        
        // Extract just the filenames (not full paths)
        for file_path in created_files {
            let filename = file_path.file_name().unwrap().to_str().unwrap().to_string();
            
            // CRITICAL CHECK: Verify no duplicate filenames across agents
            assert!(!all_files.contains(&filename), 
                "Agent {} created duplicate file: {} (collision detected!)", 
                agent_id, filename);
            
            all_files.insert(filename);
        }
    }
    
    // CRITICAL VERIFICATION: All 40 files should have unique names
    assert_eq!(all_files.len(), 40, 
        "Expected 40 unique files, got {} (indicates collision bug)", 
        all_files.len());
    
    // Verify file naming follows global indexing pattern
    for i in 0..40 {
        let expected_name = format!("file_{:08}.dat", i);
        assert!(all_files.contains(&expected_name), 
            "Missing file with global index: {}", expected_name);
    }
    
    Ok(())
}

/// Test agent assignment distribution is balanced
#[tokio::test]
async fn test_agent_assignment_balanced() -> Result<()> {
    let config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 3,
            depth: 2,
            files_per_dir: 5,
            distribution: "bottom".to_string(),
            dir_mask: "d%d_w%d.dir".to_string(),
        }),
        prepare_strategy: Default::default(),
        skip_verification: false,
    };
    
    let num_agents = 3;
    
    // Calculate expected distribution:
    // Depth 1: 3 dirs, Depth 2: 9 dirs (total 12 dirs)
    // With "bottom" distribution: only 9 leaf dirs get files
    // Total files: 9 * 5 = 45 files (fixed: comment had 9 * 6 = 54)
    // Each agent: 45 / 3 = 15 files
    
    // Create manifests for each agent to verify actual distribution
    let mut agent_file_counts = vec![0; num_agents];
    
    for agent_id in 0..num_agents {
        let temp_dir = TempDir::new()?;
        let base_uri = format!("file://{}", temp_dir.path().display());
        let mut metrics = PrepareMetrics::default();
        let manifest = create_directory_tree(&config, agent_id, num_agents, &base_uri, &mut metrics, None).await?;
        
        // Count files created by this agent
        let files = list_files_recursive(temp_dir.path())?;
        agent_file_counts[agent_id] = files.len();
        
        // Verify manifest reports correct total
        assert_eq!(manifest.total_files, 45, "Total files should be 45 (9 leaf dirs * 5 files)");
    }
    
    // Verify balanced distribution: each agent gets 15 files
    for (agent_id, count) in agent_file_counts.iter().enumerate() {
        assert_eq!(*count, 15, 
            "Agent {} should create 15 files (45 total / 3 agents), got {}", 
            agent_id, count);
    }
    
    Ok(())
}

/// Test directory distribution with exclusive mode
#[tokio::test]
async fn test_exclusive_directory_distribution() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 3,
            files_per_dir: 0,  // No files, just directories
            distribution: "bottom".to_string(),
            dir_mask: "sai3.d%d_w%d.dir".to_string(),
        }),
        prepare_strategy: Default::default(),
        skip_verification: false,
    };
    
    let num_agents = 3;
    
    // Agent 0 creates its assigned directories
    let mut metrics0 = PrepareMetrics::default();
    let manifest0 = create_directory_tree(&config, 0, num_agents, &base_uri, &mut metrics0, None).await?;
    let dirs0 = manifest0.get_agent_dirs(0);
    
    // Agent 1 creates its assigned directories (different subset)
    let mut metrics1 = PrepareMetrics::default();
    let manifest1 = create_directory_tree(&config, 1, num_agents, &base_uri, &mut metrics1, None).await?;
    let dirs1 = manifest1.get_agent_dirs(1);
    
    // Verify no overlap in assignments
    let set0: HashSet<_> = dirs0.iter().collect();
    let set1: HashSet<_> = dirs1.iter().collect();
    let intersection: Vec<_> = set0.intersection(&set1).collect();
    
    assert!(intersection.is_empty(), 
        "Agents should have exclusive directory assignments, found overlap: {:?}", 
        intersection);
    
    // Total: 2 + 4 + 8 = 14 directories
    assert_eq!(manifest0.total_dirs, 14);
    
    // Verify modulo distribution
    for (idx, dir) in manifest0.all_directories.iter().enumerate() {
        let assigned_agent = idx % num_agents;
        if assigned_agent == 0 {
            assert!(dirs0.contains(dir), 
                "Directory {} (index {}) should be assigned to agent 0", 
                dir, idx);
        }
    }
    
    Ok(())
}

/// Test "all" distribution creates files at all levels
#[tokio::test]
async fn test_all_distribution_files_at_all_levels() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 2,
            distribution: "all".to_string(),  // Files at ALL levels
            dir_mask: "level.d%d_w%d.dir".to_string(),
        }),
        prepare_strategy: Default::default(),
        skip_verification: false,
    };
    
    let mut metrics = PrepareMetrics::default();
    let manifest = create_directory_tree(&config, 0, 1, &base_uri, &mut metrics, None).await?;
    
    // 6 directories (2 + 4) * 2 files = 12 files
    assert_eq!(manifest.total_files, 12);
    
    let created_files = list_files_recursive(temp_dir.path())?;
    assert_eq!(created_files.len(), 12, "Should have 12 files at all levels");
    
    // Verify files exist at depth 1 directories
    // Structure: /tmp/.tmpXXX/level.d1_w1.dir/file_00000000.dat
    // We count components to determine depth relative to base
    let base_components = temp_dir.path().components().count();
    let depth1_files: Vec<_> = created_files.iter()
        .filter(|p| p.components().count() == base_components + 2)  // base + d1_dir + file
        .collect();
    assert!(!depth1_files.is_empty(), 
        "Files should exist at depth 1 directories (found {} at depth 1, {} total)", 
        depth1_files.len(), created_files.len());
    
    Ok(())
}

/// Test "bottom" distribution creates files only at deepest level
#[tokio::test]
async fn test_bottom_distribution_files_at_leaf_only() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        cleanup_mode: CleanupMode::Tolerant,
        cleanup_only: Some(false),
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 3,
            distribution: "bottom".to_string(),  // Fixed: was "even", should be "bottom" per test name
            dir_mask: "d%d_w%d.dir".to_string(),
        }),
        prepare_strategy: Default::default(),
        skip_verification: false,
    };
    
    let mut metrics = PrepareMetrics::default();
    let manifest = create_directory_tree(&config, 0, 1, &base_uri, &mut metrics, None).await?;
    
    // Only 4 leaf directories * 3 files = 12 files
    assert_eq!(manifest.total_files, 12);
    
    let created_files = list_files_recursive(temp_dir.path())?;
    assert_eq!(created_files.len(), 12, "Should have 12 files at leaf level only");
    
    // Calculate base path components
    let base_components = temp_dir.path().components().count();
    
    // Verify NO files at depth 1 directories
    // Structure at depth 1: /tmp/.tmpXXX/leaf.d1_w1.dir/file (would be base + 2)
    let depth1_files: Vec<_> = created_files.iter()
        .filter(|p| p.components().count() == base_components + 2)
        .collect();
    assert!(depth1_files.is_empty(), 
        "Files should NOT exist at depth 1 with 'bottom' distribution (found {} at depth 1)", 
        depth1_files.len());
    
    // Verify ALL files are at depth 2 (leaf level)
    // Structure at depth 2: /tmp/.tmpXXX/leaf.d1_w1.dir/leaf.d2_w1.dir/file (base + 3)
    let depth2_files: Vec<_> = created_files.iter()
        .filter(|p| p.components().count() == base_components + 3)
        .collect();
    assert_eq!(depth2_files.len(), 12, 
        "All 12 files should be at depth 2 (found {} at depth 2)", 
        depth2_files.len());
    
    Ok(())
}

// ============================================================================
// Helper Functions
// ============================================================================

fn list_directories(base: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut dirs = Vec::new();
    visit_dirs(base, &mut |entry| {
        if entry.path().is_dir() && entry.path() != base {
            dirs.push(entry.path().to_path_buf());
        }
    })?;
    Ok(dirs)
}

fn list_files_recursive(base: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    visit_dirs(base, &mut |entry| {
        if entry.path().is_file() {
            files.push(entry.path().to_path_buf());
        }
    })?;
    Ok(files)
}

fn visit_dirs(dir: &std::path::Path, cb: &mut dyn FnMut(&std::fs::DirEntry)) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            cb(&entry);
            if entry.path().is_dir() {
                visit_dirs(&entry.path(), cb)?;
            }
        }
    }
    Ok(())
}
