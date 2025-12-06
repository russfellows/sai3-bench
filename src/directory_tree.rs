// src/directory_tree.rs
//! Directory tree structure generation for deterministic filesystem testing.
//!
//! Inspired by rdf-bench's width/depth model, this module provides:
//! - Hierarchical directory tree generation (width^depth)
//! - Deterministic path naming (bench.{depth}_{width}.dir)
//! - Path enumeration for targeted operations
//! - Realistic filesystem layout simulation

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for hierarchical directory structure generation
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DirectoryStructureConfig {
    /// Number of subdirectories per directory level
    pub width: usize,
    
    /// Number of levels in the directory tree (1 = flat, 2+ = nested)
    pub depth: usize,
    
    /// Number of files to create in each directory
    #[serde(default)]
    pub files_per_dir: usize,
    
    /// File distribution strategy: "bottom" (only at deepest level) or "all" (at every level)
    #[serde(default = "default_distribution")]
    pub distribution: String,
    
    /// Directory naming pattern with two %d placeholders for depth and width
    /// Example: "sai3bench.d%d_w%d.dir" produces:
    ///   - "sai3bench.d1_w1.dir", "sai3bench.d1_w2.dir" (level 1)
    ///   - "sai3bench.d2_w1.dir", "sai3bench.d2_w2.dir" (level 2, under each parent)
    ///
    /// The first %d is replaced with depth (level), second %d with width (child index within parent)
    #[serde(default = "default_dir_mask")]
    pub dir_mask: String,
}

fn default_distribution() -> String {
    "bottom".to_string()
}

fn default_dir_mask() -> String {
    "sai3bench.d%d_w%d.dir".to_string()
}

/// A node in the directory tree representing a single directory
#[derive(Debug, Clone)]
pub struct DirectoryNode {
    /// Depth level (1-indexed): 1 = root level, 2 = second level, etc.
    pub depth: usize,
    
    /// Width index (1-indexed): 1 = first child, 2 = second child, etc.
    pub width: usize,
    
    /// Full path relative to anchor (includes all parent directories)
    pub full_path: String,
    
    /// Just this directory's name (without parents)
    pub dir_name: String,
    
    /// Whether this directory should contain files (based on distribution strategy)
    pub has_files: bool,
}

/// Directory tree structure with all paths pre-computed
pub struct DirectoryTree {
    config: DirectoryStructureConfig,
    
    /// All directories in the tree, indexed by "depth_width" key
    directories: HashMap<String, DirectoryNode>,
    
    /// List of all directory paths for enumeration
    all_paths: Vec<String>,
    
    /// Directories by level (for level-specific operations)
    by_level: HashMap<usize, Vec<String>>,
    
    /// Total number of directories
    total_directories: usize,
    
    /// Total number of files
    total_files: usize,
}

impl DirectoryTree {
    /// Create a new directory tree from configuration
    pub fn new(config: DirectoryStructureConfig) -> Result<Self> {
        if config.width == 0 {
            bail!("Directory width must be at least 1");
        }
        if config.depth == 0 {
            bail!("Directory depth must be at least 1");
        }
        
        let mut tree = DirectoryTree {
            config: config.clone(),
            directories: HashMap::new(),
            all_paths: Vec::new(),
            by_level: HashMap::new(),
            total_directories: 0,
            total_files: 0,
        };
        
        tree.generate()?;
        Ok(tree)
    }
    
    /// Generate the complete directory tree structure
    fn generate(&mut self) -> Result<()> {
        // Calculate total directories: width^1 + width^2 + ... + width^depth
        self.total_directories = 0;
        for level in 1..=self.config.depth {
            let dirs_at_level = self.config.width.pow(level as u32);
            self.total_directories += dirs_at_level;
        }
        
        // Generate all directory nodes
        for level in 1..=self.config.depth {
            let dirs_at_level = self.config.width.pow(level as u32);
            let mut level_paths = Vec::new();
            
            for width_idx in 1..=dirs_at_level {
                let node = self.create_node(level, width_idx)?;
                let key = format!("{}_{}", level, width_idx);
                level_paths.push(node.full_path.clone());
                self.all_paths.push(node.full_path.clone());
                self.directories.insert(key, node);
            }
            
            self.by_level.insert(level, level_paths);
        }
        
        // Calculate total files based on distribution strategy
        self.total_files = match self.config.distribution.as_str() {
            "bottom" => {
                // Files only at deepest level
                let leaf_dirs = self.config.width.pow(self.config.depth as u32);
                leaf_dirs * self.config.files_per_dir
            }
            "all" => {
                // Files at all levels
                self.total_directories * self.config.files_per_dir
            }
            _ => bail!("Invalid distribution strategy: '{}'. Must be 'bottom' or 'all'", self.config.distribution),
        };
        
        Ok(())
    }
    
    /// Create a single directory node
    fn create_node(&self, depth: usize, global_width_idx: usize) -> Result<DirectoryNode> {
        // Calculate local width relative to parent (resets for each parent)
        // For global index G at depth D with width W:
        //   local_width = ((G - 1) % W) + 1
        //   parent_index = ((G - 1) / W) + 1
        let local_width = ((global_width_idx - 1) % self.config.width) + 1;
        
        // Generate directory name using mask with LOCAL width
        // Replace first %d with depth, second %d with local_width
        let dir_name = {
            let temp = self.config.dir_mask.replacen("%d", &format!("{}", depth), 1);
            temp.replacen("%d", &format!("{}", local_width), 1)
        };
        
        // Build full path by traversing parent hierarchy
        let full_path = if depth == 1 {
            // Root level - no parents
            dir_name.clone()
        } else {
            // Calculate parent's global index: which parent at previous level
            // For global width G at depth D, parent is at depth D-1, parent_global_idx = ceil(G / config.width)
            let parent_global_idx = ((global_width_idx - 1) / self.config.width) + 1;
            let parent_key = format!("{}_{}", depth - 1, parent_global_idx);
            
            if let Some(parent) = self.directories.get(&parent_key) {
                format!("{}/{}", parent.full_path, dir_name)
            } else {
                // Parent not generated yet - build path recursively
                let parent_node = self.create_node(depth - 1, parent_global_idx)?;
                format!("{}/{}", parent_node.full_path, dir_name)
            }
        };
        
        // Determine if this directory should have files
        let has_files = match self.config.distribution.as_str() {
            "bottom" => depth == self.config.depth,
            "all" => true,
            _ => false,
        };
        
        Ok(DirectoryNode {
            depth,
            width: local_width,
            full_path,
            dir_name,
            has_files,
        })
    }
    
    /// Get all directory paths
    pub fn all_paths(&self) -> &[String] {
        &self.all_paths
    }
    
    /// Get directory paths at a specific level
    pub fn paths_at_level(&self, level: usize) -> Option<&[String]> {
        self.by_level.get(&level).map(|v| v.as_slice())
    }
    
    /// Get total number of directories
    pub fn total_directories(&self) -> usize {
        self.total_directories
    }
    
    /// Get total number of files
    pub fn total_files(&self) -> usize {
        self.total_files
    }
    
    /// Get configuration
    pub fn config(&self) -> &DirectoryStructureConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_directory_count_calculation() {
        // width=4, depth=3 → 4^1 + 4^2 + 4^3 = 4 + 16 + 64 = 84
        let config = DirectoryStructureConfig {
            width: 4,
            depth: 3,
            files_per_dir: 0,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        assert_eq!(tree.total_directories(), 84);
        
        // Verify each level has correct count
        assert_eq!(tree.paths_at_level(1).unwrap().len(), 4);   // 4^1 = 4
        assert_eq!(tree.paths_at_level(2).unwrap().len(), 16);  // 4^2 = 16
        assert_eq!(tree.paths_at_level(3).unwrap().len(), 64);  // 4^3 = 64
    }
    
    #[test]
    fn test_flat_structure() {
        // width=10, depth=1 → 10 directories
        let config = DirectoryStructureConfig {
            width: 10,
            depth: 1,
            files_per_dir: 0,
            distribution: "bottom".to_string(),
            dir_mask: "dir%d_%d".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        assert_eq!(tree.total_directories(), 10);
        assert_eq!(tree.all_paths().len(), 10);
        
        // All paths should be at level 1
        assert_eq!(tree.paths_at_level(1).unwrap().len(), 10);
        assert!(tree.paths_at_level(2).is_none());
    }
    
    #[test]
    fn test_file_distribution_bottom() {
        // width=2, depth=2, files_per_dir=5, distribution=bottom
        // Directories: 2 + 4 = 6
        // Files: only at depth 2 → 4 dirs * 5 files = 20
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 5,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        assert_eq!(tree.total_directories(), 6);
        assert_eq!(tree.total_files(), 20);
        
        // Verify only bottom level has files
        let manifest = TreeManifest::from_tree(&tree);
        assert_eq!(manifest.file_ranges.len(), 4);  // Only 4 dirs at depth 2 have files
        // Check that all dirs with files are at depth 2 (have 1 slash)
        for (dir_path, _) in &manifest.file_ranges {
            assert_eq!(dir_path.matches('/').count(), 1, "Dir {} should be at depth 2", dir_path);
        }
    }
    
    #[test]
    fn test_file_distribution_all() {
        // width=2, depth=2, files_per_dir=5, distribution=all
        // Directories: 2 + 4 = 6
        // Files: all dirs → 6 dirs * 5 files = 30
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 5,
            distribution: "all".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        assert_eq!(tree.total_directories(), 6);
        assert_eq!(tree.total_files(), 30);
        
        // Verify all directories have files
        let manifest = TreeManifest::from_tree(&tree);
        assert_eq!(manifest.file_ranges.len(), 6);  // All 6 directories should have files
    }
    
    #[test]
    fn test_directory_naming() {
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 0,
            distribution: "bottom".to_string(),
            dir_mask: "test.%d_%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        
        // Level 1 should have: test.1_1.dir, test.1_2.dir
        let level1 = tree.paths_at_level(1).unwrap();
        assert_eq!(level1.len(), 2);
        assert!(level1.contains(&"test.1_1.dir".to_string()));
        assert!(level1.contains(&"test.1_2.dir".to_string()));
        
        // Level 2 should have nested paths
        let level2 = tree.paths_at_level(2).unwrap();
        assert_eq!(level2.len(), 4);
        assert!(level2.iter().any(|p| p.contains("test.1_1.dir/test.2_")));
    }
    
    #[test]
    fn test_actual_path_structure() {
        // Small tree to manually verify structure
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 0,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        
        println!("\n=== Directory Structure (width=2, depth=2) ===");
        for level in 1..=2 {
            println!("\nLevel {level}:");
            for path in tree.paths_at_level(level).unwrap() {
                println!("  {path}");
            }
        }
        
        // Verify level 1
        let level1 = tree.paths_at_level(1).unwrap();
        assert_eq!(level1, vec!["sai3bench.d1_w1.dir", "sai3bench.d1_w2.dir"]);
        
        // Verify level 2 paths are nested correctly
        let level2 = tree.paths_at_level(2).unwrap();
        assert_eq!(level2.len(), 4);
        
        // First 2 children should be under sai3bench.d1_w1.dir
        assert!(level2[0].starts_with("sai3bench.d1_w1.dir/"));
        assert!(level2[1].starts_with("sai3bench.d1_w1.dir/"));
        
        // Last 2 children should be under sai3bench.d1_w2.dir
        assert!(level2[2].starts_with("sai3bench.d1_w2.dir/"));
        assert!(level2[3].starts_with("sai3bench.d1_w2.dir/"));
    }
    
    #[test]
    fn test_invalid_config() {
        // Zero width should fail
        let config = DirectoryStructureConfig {
            width: 0,
            depth: 1,
            files_per_dir: 0,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        assert!(DirectoryTree::new(config).is_err());
        
        // Zero depth should fail
        let config = DirectoryStructureConfig {
            width: 1,
            depth: 0,
            files_per_dir: 0,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        assert!(DirectoryTree::new(config).is_err());
        
        // Invalid distribution should fail
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 5,
            distribution: "invalid".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        assert!(DirectoryTree::new(config).is_err());
    }
    
    #[test]
    fn test_large_tree() {
        // width=10, depth=3 → 10 + 100 + 1000 = 1,110 directories
        let config = DirectoryStructureConfig {
            width: 10,
            depth: 3,
            files_per_dir: 100,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        assert_eq!(tree.total_directories(), 1110);
        assert_eq!(tree.total_files(), 100_000);  // 1000 leaf dirs * 100 files
        
        println!("\n=== Large Tree Stats ===");
        println!("Total directories: {}", tree.total_directories());
        println!("Total files: {}", tree.total_files());
        println!("Level 1 dirs: {}", tree.paths_at_level(1).unwrap().len());
        println!("Level 2 dirs: {}", tree.paths_at_level(2).unwrap().len());
        println!("Level 3 dirs: {}", tree.paths_at_level(3).unwrap().len());
    }
    
    #[test]
    fn test_files_per_directory_bottom() {
        // width=3, depth=3, files_per_dir=10, distribution=bottom
        // Only directories at depth 3 should have files
        let config = DirectoryStructureConfig {
            width: 3,
            depth: 3,
            files_per_dir: 10,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        
        // Total: 3 + 9 + 27 = 39 directories
        assert_eq!(tree.total_directories(), 39);
        
        // Files only at depth 3: 27 dirs * 10 files = 270
        assert_eq!(tree.total_files(), 270);
        
        // Verify has_files flag by level
        for (key, node) in &tree.directories {
            if node.depth == 3 {
                // Bottom level - should have files
                assert!(node.has_files, 
                    "Directory {} at depth 3 should have files", key);
            } else {
                // Non-bottom levels - should NOT have files
                assert!(!node.has_files, 
                    "Directory {} at depth {} should NOT have files", key, node.depth);
            }
        }
        
        // Verify manifest shows only depth 3 directories have files
        let manifest = TreeManifest::from_tree(&tree);
        assert_eq!(manifest.file_ranges.len(), 27, "Should have exactly 27 directories with files");
        for (dir_path, _) in &manifest.file_ranges {
            assert_eq!(dir_path.matches('/').count(), 2, 
                "All file directories should be at depth 3 (2 slashes), but {} has {}", 
                dir_path, dir_path.matches('/').count());
        }
    }
    
    #[test]
    fn test_files_per_directory_all() {
        // width=2, depth=3, files_per_dir=15, distribution=all
        // ALL directories should have files
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 3,
            files_per_dir: 15,
            distribution: "all".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        
        // Total: 2 + 4 + 8 = 14 directories
        assert_eq!(tree.total_directories(), 14);
        
        // Files at all levels: 14 dirs * 15 files = 210
        assert_eq!(tree.total_files(), 210);
        
        // Verify ALL directories have has_files=true
        for (key, node) in &tree.directories {
            assert!(node.has_files, 
                "Directory {} at depth {} should have files with 'all' distribution", 
                key, node.depth);
        }
        
        // Verify manifest shows all directories have files
        let manifest = TreeManifest::from_tree(&tree);
        assert_eq!(manifest.file_ranges.len(), 14, "All 14 directories should have files");
        
        // Verify representation across all levels
        let level1_with_files = manifest.file_ranges.iter().filter(|(p, _)| p.matches('/').count() == 0).count();
        let level2_with_files = manifest.file_ranges.iter().filter(|(p, _)| p.matches('/').count() == 1).count();
        let level3_with_files = manifest.file_ranges.iter().filter(|(p, _)| p.matches('/').count() == 2).count();
        
        assert_eq!(level1_with_files, 2, "Level 1 should have 2 dirs with files");
        assert_eq!(level2_with_files, 4, "Level 2 should have 4 dirs with files");
        assert_eq!(level3_with_files, 8, "Level 3 should have 8 dirs with files");
    }
}

/// Serializable tree manifest for coordination between agents
/// Provides a shared logical map of the complete directory/file structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeManifest {
    /// All directory paths in the tree (relative to anchor)
    pub all_directories: Vec<String>,
    
    /// Directories grouped by depth level (1-indexed)
    pub by_level: HashMap<usize, Vec<String>>,
    
    /// For partitioned/exclusive modes: which agent owns which directories
    /// Key is agent_id (0-indexed), value is list of directory paths
    #[serde(default)]
    pub agent_assignments: HashMap<usize, Vec<String>>,
    
    /// Configuration hash for validation (ensures all agents use same config)
    pub config_hash: String,
    
    /// Total directory count
    pub total_dirs: usize,
    
    /// Total file count (based on distribution strategy)
    pub total_files: usize,
    
    /// Files per directory (from config)
    pub files_per_dir: usize,
    
    /// Distribution strategy: "bottom" or "all"
    pub distribution: String,
    
    /// File index ranges per directory: Vec<(dir_path, (start_idx, end_idx))>
    /// Stored in same order as all_directories for consistency
    /// Example: [("d1_w1.dir/d2_w1.dir", (0, 5)), ("d1_w1.dir/d2_w2.dir", (5, 10))]
    #[serde(default)]
    pub file_ranges: Vec<(String, (usize, usize))>,
}

impl TreeManifest {
    /// Create manifest from a DirectoryTree
    pub fn from_tree(tree: &DirectoryTree) -> Self {
        let config_hash = Self::hash_config(&tree.config);
        
        let mut manifest = TreeManifest {
            all_directories: tree.all_paths.clone(),
            by_level: tree.by_level.clone(),
            agent_assignments: HashMap::new(),  // Filled in by coordinator
            config_hash,
            total_dirs: tree.total_directories,
            total_files: tree.total_files,
            files_per_dir: tree.config.files_per_dir,
            distribution: tree.config.distribution.clone(),
            file_ranges: Vec::new(),
        };
        
        // Compute file ranges immediately
        manifest.compute_file_ranges();
        
        manifest
    }
    
    /// Compute file distribution across directories
    /// Creates global file index ranges for each directory based on distribution strategy
    pub fn compute_file_ranges(&mut self) {
        self.file_ranges.clear();
        let mut global_idx = 0;
        
        // Determine max depth for "bottom" distribution
        let max_depth = if let Some(max_level) = self.by_level.keys().max() {
            *max_level
        } else {
            return; // No directories
        };
        
        for dir_path in &self.all_directories {
            let should_have_files = match self.distribution.as_str() {
                "bottom" => {
                    // Only deepest level directories have files
                    // Count slashes to determine depth
                    let depth = dir_path.matches('/').count() + 1;
                    depth == max_depth
                }
                "all" => true,
                _ => false,
            };
            
            if should_have_files && self.files_per_dir > 0 {
                let start_idx = global_idx;
                let end_idx = global_idx + self.files_per_dir;
                self.file_ranges.push((dir_path.clone(), (start_idx, end_idx)));
                global_idx = end_idx;
            }
        }
        
        // Update total_files based on actual file ranges
        self.total_files = global_idx;
    }
    
    /// Get file name for a global file index
    /// Returns standardized file name: "file_00000000.dat"
    pub fn get_file_name(&self, global_idx: usize) -> String {
        format!("file_{:08}.dat", global_idx)
    }
    
    /// Helper: Look up file range for a specific directory
    /// Returns None if directory has no files
    pub fn get_file_range(&self, dir_path: &str) -> Option<&(usize, usize)> {
        self.file_ranges
            .iter()
            .find(|(path, _)| path == dir_path)
            .map(|(_, range)| range)
    }
    
    /// Get full relative path for a global file index
    /// Returns: "d1_w1.dir/d2_w1.dir/file_00000000.dat"
    /// Returns None if global_idx is out of range
    pub fn get_file_path(&self, global_idx: usize) -> Option<String> {
        // Iterate file_ranges in order (matches creation order)
        for (dir_path, (start, end)) in &self.file_ranges {
            if global_idx >= *start && global_idx < *end {
                let file_name = self.get_file_name(global_idx);
                return Some(format!("{}/{}", dir_path, file_name));
            }
        }
        None
    }
    
    /// Get list of all files in a specific directory
    /// Returns empty vec if directory has no files
    pub fn get_files_in_directory(&self, dir_path: &str) -> Vec<String> {
        if let Some((start, end)) = self.get_file_range(dir_path) {
            (*start..*end)
                .map(|idx| self.get_file_name(idx))
                .collect()
        } else {
            Vec::new()
        }
    }
    
    /// Compute agent assignments for exclusive or partitioned modes
    pub fn assign_agents(&mut self, num_agents: usize) {
        if num_agents == 0 {
            return;
        }
        
        self.agent_assignments.clear();
        
        // Distribute directories evenly across agents
        for (idx, path) in self.all_directories.iter().enumerate() {
            let agent_id = idx % num_agents;
            self.agent_assignments
                .entry(agent_id)
                .or_default()
                .push(path.clone());
        }
    }
    
    /// Get directories assigned to a specific agent
    pub fn get_agent_dirs(&self, agent_id: usize) -> Vec<String> {
        self.agent_assignments
            .get(&agent_id)
            .cloned()
            .unwrap_or_default()
    }
    
    /// Determine if an agent should handle a specific file (for distributed cleanup)
    /// Uses modulo distribution: file_idx % num_agents == agent_id
    /// This ensures deterministic, non-overlapping assignment across agents
    pub fn should_agent_handle_file(&self, global_file_idx: usize, agent_id: usize, num_agents: usize) -> bool {
        if num_agents <= 1 {
            return true;  // Single agent handles all files
        }
        global_file_idx % num_agents == agent_id
    }
    
    /// Get all file indices assigned to a specific agent (for distributed cleanup)
    /// Returns indices in ascending order for efficient iteration
    pub fn get_agent_file_indices(&self, agent_id: usize, num_agents: usize) -> Vec<usize> {
        if num_agents <= 1 {
            return (0..self.total_files).collect();
        }
        
        (0..self.total_files)
            .filter(|&idx| idx % num_agents == agent_id)
            .collect()
    }
    
    /// Get all file paths assigned to a specific agent (for distributed cleanup)
    /// Returns full relative paths like "d1_w1.dir/d2_w1.dir/file_00000000.dat"
    pub fn get_agent_file_paths(&self, agent_id: usize, num_agents: usize) -> Vec<String> {
        self.get_agent_file_indices(agent_id, num_agents)
            .into_iter()
            .filter_map(|idx| self.get_file_path(idx))
            .collect()
    }
    
    /// Validate config hash matches expected
    pub fn validate_config(&self, config: &DirectoryStructureConfig) -> bool {
        Self::hash_config(config) == self.config_hash
    }
    
    /// Compute deterministic hash of config for validation
    fn hash_config(config: &DirectoryStructureConfig) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        config.width.hash(&mut hasher);
        config.depth.hash(&mut hasher);
        config.files_per_dir.hash(&mut hasher);
        config.distribution.hash(&mut hasher);
        config.dir_mask.hash(&mut hasher);
        
        format!("{:x}", hasher.finish())
    }
    
    /// Serialize manifest to JSON string
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize TreeManifest: {}", e))
    }
    
    /// Deserialize manifest from JSON string
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize TreeManifest: {}", e))
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;
    
    #[test]
    fn test_manifest_from_tree() {
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 10,
            distribution: "bottom".to_string(),
            dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        let manifest = TreeManifest::from_tree(&tree);
        
        assert_eq!(manifest.total_dirs, 6);  // 2 + 4 = 6
        assert_eq!(manifest.total_files, 40);  // 4 leaf dirs * 10 files
        assert_eq!(manifest.all_directories.len(), 6);
        assert_eq!(manifest.by_level.len(), 2);
        assert_eq!(manifest.files_per_dir, 10);
        assert_eq!(manifest.distribution, "bottom");
    }
    
    #[test]
    fn test_manifest_agent_assignment() {
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 5,
            distribution: "all".to_string(),
            dir_mask: "bench.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config).unwrap();
        let mut manifest = TreeManifest::from_tree(&tree);
        
        // Assign to 3 agents
        manifest.assign_agents(3);
        
        // Verify assignments
        assert_eq!(manifest.agent_assignments.len(), 3);
        
        let agent0_dirs = manifest.get_agent_dirs(0);
        let agent1_dirs = manifest.get_agent_dirs(1);
        let agent2_dirs = manifest.get_agent_dirs(2);
        
        // Total should equal all directories
        let total_assigned = agent0_dirs.len() + agent1_dirs.len() + agent2_dirs.len();
        assert_eq!(total_assigned, 6);
        
        // Each agent gets 2 directories (6 / 3)
        assert_eq!(agent0_dirs.len(), 2);
        assert_eq!(agent1_dirs.len(), 2);
        assert_eq!(agent2_dirs.len(), 2);
    }
    
    #[test]
    fn test_manifest_serialization() {
        let config = DirectoryStructureConfig {
            width: 2,
            depth: 1,
            files_per_dir: 3,
            distribution: "all".to_string(),
            dir_mask: "dir.d%d_w%d".to_string(),
        };
        
        let tree = DirectoryTree::new(config.clone()).unwrap();
        let manifest = TreeManifest::from_tree(&tree);
        
        // Serialize to JSON
        let json = manifest.to_json().unwrap();
        assert!(json.contains("all_directories"));
        assert!(json.contains("total_dirs"));
        
        // Deserialize back
        let manifest2 = TreeManifest::from_json(&json).unwrap();
        assert_eq!(manifest2.total_dirs, manifest.total_dirs);
        assert_eq!(manifest2.total_files, manifest.total_files);
        assert_eq!(manifest2.all_directories, manifest.all_directories);
    }
    
    #[test]
    fn test_manifest_config_validation() {
        let config = DirectoryStructureConfig {
            width: 3,
            depth: 2,
            files_per_dir: 7,
            distribution: "bottom".to_string(),
            dir_mask: "test.d%d_w%d.dir".to_string(),
        };
        
        let tree = DirectoryTree::new(config.clone()).unwrap();
        let manifest = TreeManifest::from_tree(&tree);
        
        // Same config should validate
        assert!(manifest.validate_config(&config));
        
        // Different config should not validate
        let mut different_config = config.clone();
        different_config.width = 4;
        assert!(!manifest.validate_config(&different_config));
    }
}
