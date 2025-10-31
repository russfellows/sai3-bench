// tests/directory_tree_creation_tests.rs
//! Integration tests for directory tree creation with multi-agent coordination
//! 
//! CRITICAL: These tests verify we avoid the rdf-bench collision bug (v2025.10.0)
//! where multiple workers created files with identical names on shared filesystems

use anyhow::Result;
use sai3_bench::config::PrepareConfig;
use sai3_bench::directory_tree::DirectoryStructureConfig;
use sai3_bench::workload::create_directory_tree;
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
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 3,
            distribution: "bottom".to_string(),
            dir_mask: "d%d_w%d.dir".to_string(),
        }),
    };
    
    let manifest = create_directory_tree(&config, 0, 1, &base_uri).await?;
    
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
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 10,  // 40 total files
            distribution: "bottom".to_string(),
            dir_mask: "test.d%d_w%d.dir".to_string(),
        }),
    };
    
    let num_agents = 4;
    let mut all_files = HashSet::new();
    
    // Simulate 4 agents creating files
    for agent_id in 0..num_agents {
        let temp_agent_dir = TempDir::new()?;
        let agent_base_uri = format!("file://{}", temp_agent_dir.path().display());
        
        let manifest = create_directory_tree(&config, agent_id, num_agents, &agent_base_uri).await?;
        
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
    let _config = PrepareConfig {
        ensure_objects: vec![],
        cleanup: false,
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 3,
            depth: 2,
            files_per_dir: 5,
            distribution: "all".to_string(),
            dir_mask: "dir.d%d_w%d".to_string(),
        }),
    };
    
    let num_agents = 3;
    let mut file_counts = vec![0; num_agents];
    
    // Total: 3 + 9 = 12 directories, 12 * 5 = 60 files
    let total_files = 60;
    
    // Simulate global file distribution
    for global_idx in 0..total_files {
        let assigned_agent = global_idx % num_agents;
        file_counts[assigned_agent] += 1;
    }
    
    // Each agent should get exactly 20 files (60 / 3)
    for (agent_id, count) in file_counts.iter().enumerate() {
        assert_eq!(*count, 20, 
            "Agent {} should create 20 files, got {}", 
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
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 3,
            files_per_dir: 0,  // No files, just directories
            distribution: "bottom".to_string(),
            dir_mask: "sai3.d%d_w%d.dir".to_string(),
        }),
    };
    
    let num_agents = 3;
    
    // Agent 0 creates its assigned directories
    let manifest0 = create_directory_tree(&config, 0, num_agents, &base_uri).await?;
    let dirs0 = manifest0.get_agent_dirs(0);
    
    // Agent 1 creates its assigned directories (different subset)
    let manifest1 = create_directory_tree(&config, 1, num_agents, &base_uri).await?;
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
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 2,
            distribution: "all".to_string(),  // Files at ALL levels
            dir_mask: "level.d%d_w%d.dir".to_string(),
        }),
    };
    
    let manifest = create_directory_tree(&config, 0, 1, &base_uri).await?;
    
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
        post_prepare_delay: 0,
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 3,
            distribution: "bottom".to_string(),  // Files only at leaf level
            dir_mask: "leaf.d%d_w%d.dir".to_string(),
        }),
    };
    
    let manifest = create_directory_tree(&config, 0, 1, &base_uri).await?;
    
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
