//! Directory tree creation and path selection
//!
//! Handles creation of nested directory structures for file-based workloads,
//! including distributed mode where directories are partitioned across agents.

use std::sync::Arc;
use std::time::Instant;
use anyhow::{anyhow, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{debug, info, warn};

use crate::config::{FillPattern, PrepareConfig};
use crate::directory_tree::TreeManifest;
use crate::workload::create_store_for_uri;
use super::metrics::PrepareMetrics;

pub fn create_tree_manifest_only(
    config: &PrepareConfig,
    _agent_id: usize,
    num_agents: usize,
    _base_uri: &str,
    live_stats_tracker: Option<&crate::live_stats::LiveStatsTracker>,
) -> Result<TreeManifest> {
    use crate::directory_tree::DirectoryTree;
    
    let dir_config = config.directory_structure.as_ref()
        .ok_or_else(|| anyhow!("No directory_structure specified in PrepareConfig"))?;
    
    info!("Creating directory tree: width={}, depth={}, files_per_dir={}, distribution={}", 
        dir_config.width, dir_config.depth, dir_config.files_per_dir, dir_config.distribution);
    
    // Generate tree structure with progress reporting
    let tree = DirectoryTree::new_with_progress(dir_config.clone(), live_stats_tracker)
        .context("Failed to create DirectoryTree")?;
    
    // Create manifest with agent assignments
    let mut manifest = TreeManifest::from_tree(&tree);
    manifest.assign_agents(num_agents);
    
    Ok(manifest)
}

/// Create directories after files have been created
/// v0.7.9: Split from create_directory_tree to support file creation first, mkdir second
pub(crate) async fn finalize_tree_with_mkdir(
    config: &PrepareConfig,
    base_uri: &str,
    metrics: &mut PrepareMetrics,
    _live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<()> {
    let dir_config = config.directory_structure.as_ref()
        .ok_or_else(|| anyhow!("No directory_structure specified"))?;
    
    // Create ObjectStore for this base URI
    let store = create_store_for_uri(base_uri)?;
    
    // Determine if backend requires explicit directory creation
    // Object storage (S3/Azure/GCS) doesn't need mkdir - directories are implicit in object keys
    // File systems (file://, direct://) need explicit mkdir
    let needs_mkdir = base_uri.starts_with("file://") || base_uri.starts_with("direct://");
    
    if needs_mkdir {
        use crate::directory_tree::DirectoryTree;
        let tree = DirectoryTree::new(dir_config.clone())?;
        let manifest = TreeManifest::from_tree(&tree);
        
        info!("Creating {} directories...", manifest.all_directories.len());
        let pb = ProgressBar::new(manifest.all_directories.len() as u64);
        pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} dirs {msg}"
        )?);
        
        for (idx, dir_path) in manifest.all_directories.iter().enumerate() {
            let full_uri = if base_uri.ends_with('/') {
                format!("{}{}", base_uri, dir_path)
            } else {
                format!("{}/{}", base_uri, dir_path)
            };
            
            store.mkdir(&full_uri).await
                .with_context(|| format!("Failed to create directory: {}", full_uri))?;
            
            metrics.mkdir_count += 1;
            metrics.mkdir.ops += 1;
            
            pb.inc(1);
            
            // Report progress to controller every 5000 directories
            if idx > 0 && idx % 5000 == 0 {
                if let Some(tracker) = &_live_stats_tracker {
                    tracker.set_stage_progress(idx as u64);
                    info!("  Directory creation progress: {}/{} ({:.1}%)",
                        idx, manifest.all_directories.len(),
                        (idx as f64 / manifest.all_directories.len() as f64) * 100.0);
                }
            }
        }
        
        pb.finish_with_message("directories created");
    } else {
        info!("Skipping directory creation for object storage (directories are implicit in object keys)");
    }
    
    Ok(())
}

/// Create directory tree structure with optional file population
/// 
/// v0.7.0: Supports distributed agent coordination with proper file indexing
/// v0.7.2: Collects mkdir metrics during directory creation
pub async fn create_directory_tree(
    config: &PrepareConfig,
    agent_id: usize,
    num_agents: usize,
    base_uri: &str,
    metrics: &mut PrepareMetrics,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<TreeManifest> {
    use crate::directory_tree::DirectoryTree;
    
    let dir_config = config.directory_structure.as_ref()
        .ok_or_else(|| anyhow!("No directory_structure specified in PrepareConfig"))?;
    
    info!("Creating directory tree: width={}, depth={}, files_per_dir={}, distribution={}", 
        dir_config.width, dir_config.depth, dir_config.files_per_dir, dir_config.distribution);
    
    // 1. Generate tree structure
    let tree = DirectoryTree::new(dir_config.clone())
        .context("Failed to create DirectoryTree")?;
    
    // 2. Create manifest with agent assignments
    let mut manifest = TreeManifest::from_tree(&tree);
    manifest.assign_agents(num_agents);
    
    info!("Tree structure: {} directories, {} files total", 
        manifest.total_dirs, manifest.total_files);
    
    if num_agents > 1 {
        let my_dirs = manifest.get_agent_dirs(agent_id);
        info!("Agent {}/{}: Assigned {} directories", 
            agent_id, num_agents, my_dirs.len());
    }
    
    // 3. Create ObjectStore for this base URI
    let store = create_store_for_uri(base_uri)?;
    
    // 4. Get directories this agent should create
    let dirs_to_create = if num_agents == 1 {
        // Single agent - create all directories
        manifest.all_directories.clone()
    } else {
        // Multiple agents - only create assigned directories
        manifest.get_agent_dirs(agent_id)
    };
    
    // 5. Create directories
    if !dirs_to_create.is_empty() {
        // Determine if backend requires explicit directory creation
        // Object storage (S3/Azure/GCS) doesn't need mkdir - directories are implicit in object keys
        // File systems (file://, direct://) need explicit mkdir
        let needs_mkdir = base_uri.starts_with("file://") || base_uri.starts_with("direct://");
        
        if needs_mkdir {
            info!("Creating {} directories...", dirs_to_create.len());
            let pb = ProgressBar::new(dirs_to_create.len() as u64);
            pb.set_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} dirs {msg}"
            )?);
            
            for dir_path in &dirs_to_create {
                let full_uri = if base_uri.ends_with('/') {
                    format!("{}{}", base_uri, dir_path)
                } else {
                    format!("{}/{}", base_uri, dir_path)
                };
                
                let mkdir_start = Instant::now();
                store.mkdir(&full_uri).await
                    .with_context(|| format!("Failed to create directory: {}", full_uri))?;
                let _mkdir_latency_us = mkdir_start.elapsed().as_micros() as u64;
                
                // Update mkdir metrics (treating mkdir as metadata operation)
                // TODO: Could track mkdir latencies in future version if needed
                metrics.mkdir_count += 1;
                // For mkdir, we don't track per-size since it's always zero-byte metadata
                // Just accumulate total latency for mean calculation later
                metrics.mkdir.bytes += 0;  // Directories have no size
                metrics.mkdir.ops += 1;
                
                pb.inc(1);
            }
            
            pb.finish_with_message("directories created");
        } else {
            info!("Skipping directory creation for object storage (directories are implicit in object keys)");
        }
    }
    
    // 6. Create files if specified
    if manifest.files_per_dir > 0 {
        // CRITICAL: Use global file indexing to avoid rdf-bench collision bug
        // Each file gets a unique global index, then modulo distribution assigns to agents
        
        // Get list of directories that have files (in consistent order from manifest)
        let dirs_with_files: Vec<&String> = manifest.file_ranges
            .iter()
            .map(|(dir, _)| dir)
            .collect();
        let total_files = manifest.total_files;
        
        info!("Verifying {} files across {} directories...", 
            total_files, dirs_with_files.len());
        
        // Step 1: Build set of expected file paths
        info!("Building expected file list...");
        let mut expected_files = std::collections::HashSet::new();
        for global_idx in 0..total_files {
            if let Some(file_path) = manifest.get_file_path(global_idx) {
                expected_files.insert(file_path);
            }
        }
        info!("  Expected {} files in tree structure", expected_files.len());
        
        // Step 2: List existing files in all directories
        info!("Checking existing files in {} directories...", dirs_with_files.len());
        let mut existing_files = std::collections::HashSet::new();
        let list_pb = ProgressBar::new(dirs_with_files.len() as u64);
        list_pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} dirs {msg}"
        )?);
        list_pb.set_message("listing");
        
        for dir_path in &dirs_with_files {
            // For GCS/S3 object storage, list files with directory prefix + "/"
            // This tells the storage to list objects whose keys start with this prefix
            let dir_prefix = if base_uri.ends_with('/') {
                format!("{}{}/", base_uri, dir_path)
            } else {
                format!("{}/{}/", base_uri, dir_path)
            };
            
            // List files with this directory prefix
            // recursive=false to list only direct children (no subdirs)
            // This avoids pagination issues with large directory trees
            
            // DEBUG: Log the exact parameters we're passing
            debug!("BEFORE list() call - dir_prefix: '{}', recursive: false", dir_prefix);
            
            // Add a small delay to avoid overwhelming GCS API
            tokio::time::sleep(tokio::time::Duration::from_millis(crate::constants::API_RATE_LIMIT_DELAY_MS)).await;
            
            match store.list(&dir_prefix, false).await {
                Ok(files) => {
                    // With recursive=false, we get only direct children
                    // No filtering needed since there are no subdirectories
                    
                    debug!("AFTER list() call - returned {} files for {}", files.len(), dir_prefix);
                    
                    // DEBUG: For problematic directories (< 130 files), show ALL files returned
                    if files.len() < 130 && !files.is_empty() {
                        warn!("⚠️  Directory {} returned only {} files (expected 130):", dir_prefix, files.len());
                        for (i, f) in files.iter().enumerate().take(10) {
                            warn!("    File {}: {}", i, f);
                        }
                        if files.len() > 10 {
                            warn!("    ... and {} more", files.len() - 10);
                        }
                    } else if files.is_empty() {
                        warn!("❌ Directory {} returned ZERO files (expected 130)", dir_prefix);
                    }
                    
                    if !files.is_empty() && files.len() <= 3 {
                        debug!("  Files: {:?}", files);
                    } else if !files.is_empty() {
                        debug!("  First file example: {}", files[0]);
                    }
                    
                    for file_uri in files {
                        // Extract relative path from full URI
                        let relative_path = if let Some(stripped) = file_uri.strip_prefix(base_uri) {
                            stripped.trim_start_matches('/')
                        } else {
                            warn!("File URI doesn't match base_uri: file={}, base={}", file_uri, base_uri);
                            continue;
                        };
                        
                        existing_files.insert(relative_path.to_string());
                    }
                }
                Err(e) => {
                    // Directory might not exist yet - that's OK, we'll create files later
                    debug!("Could not list directory {}: {}", dir_prefix, e);
                }
            }
            list_pb.inc(1);
        }
        list_pb.finish_with_message(format!("{} files found", existing_files.len()));
        
        // Step 3: Find missing files
        let missing_files: Vec<String> = expected_files
            .difference(&existing_files)
            .cloned()
            .collect();
        
        info!("  Found {} existing files, {} missing files", 
            existing_files.len(), missing_files.len());
        
        // Step 4: Create only missing files (or all if none exist)
        let to_create = missing_files.len();
        
        if to_create == 0 {
            info!("All directory tree files already exist - skipping creation");
        } else {
            info!("Creating {} missing files...", to_create);
        
        // Get file generation configuration from ensure_objects (if configured)
        // Use same pattern as regular prepare_objects for consistency
        let (size_spec, fill_pattern, dedup_factor, compress_factor) = 
            if let Some(ensure_spec) = config.ensure_objects.first() {
                (
                    ensure_spec.get_size_spec(),
                    ensure_spec.fill,
                    ensure_spec.dedup_factor,
                    ensure_spec.compress_factor,
                )
            } else {
                // Default: 1KB fixed size, zero fill, no dedup/compression
                use crate::size_generator::SizeSpec;
                (SizeSpec::Fixed(1024), FillPattern::Zero, 1, 1)
            };
        
        // Create size generator
        use crate::size_generator::SizeGenerator;
        let mut size_generator = SizeGenerator::new(&size_spec)
            .context("Failed to create size generator for tree files")?;
        
            info!("File size: {}, fill: {:?}, dedup: {}, compress: {}", 
                size_generator.description(), fill_pattern, dedup_factor, compress_factor);
            
            let pb = ProgressBar::new(to_create as u64);
            pb.set_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} files {msg}"
            )?);
            
            let mut global_file_idx = 0usize;
            
            for dir_path in dirs_with_files {
                let dir_uri = if base_uri.ends_with('/') {
                    format!("{}{}", base_uri, dir_path)
                } else {
                    format!("{}/{}", base_uri, dir_path)
                };
                
                // Create files_per_dir files in this directory
                for _local_idx in 0..manifest.files_per_dir {
                    // Check if this file is missing
                    let file_name = manifest.get_file_name(global_file_idx);
                    let file_relative_path = format!("{}/{}", dir_path, file_name);
                    let file_uri = format!("{}/{}", dir_uri, file_name);
                    
                    // Only create if this file is in the missing list
                    if missing_files.contains(&file_relative_path) {
                        // CORRECT PATTERN: Check if this global file index belongs to this agent
                        let assigned_agent = global_file_idx % num_agents;
                        
                        if assigned_agent == agent_id {
                            // This file belongs to us - create it
                            
                            // Generate file data using EXACT same pattern as regular prepare_objects
                            // OPTIMIZED v0.8.20+: Use cached generator pool for 50+ GB/s
                            let size = size_generator.generate();
                            let data = match fill_pattern {
                                FillPattern::Zero => {
                                    let buf = bytes::BytesMut::zeroed(size as usize);
                                    buf.freeze()  // Zero-copy: BytesMut→Bytes
                                }
                                FillPattern::Random => {
                                    // Already returns Bytes - zero-copy
                                    crate::data_gen_pool::generate_data_optimized(size as usize, dedup_factor, compress_factor)
                                }
                                FillPattern::Prand => {
                                    // Zero-copy data generation using BytesMut→Bytes pattern
                                    #[allow(unused_mut)]  // Suppress false warning - mut required for fill_controlled_data
                                    let mut buf = bytes::BytesMut::zeroed(size as usize);
                                    s3dlio::fill_controlled_data(&mut buf, dedup_factor, compress_factor);
                                    buf.freeze()
                                }
                            };
                            
                            let put_start = Instant::now();
                            store.put(&file_uri, data).await  // Zero-copy: Bytes passed directly
                                .with_context(|| format!("Failed to create file: {}", file_uri))?;
                            let latency = put_start.elapsed();
                            
                            // Record stats for live streaming (if tracker provided)
                            if let Some(ref tracker) = live_stats_tracker {
                                tracker.record_put(size as usize, latency);
                            }
                            
                            pb.inc(1);
                        }
                    }
                    
                    // CRITICAL: Always increment global index, even if we skip this file
                    global_file_idx += 1;
                }
            }
            
            pb.finish_with_message(format!("missing files created (agent {}/{})", agent_id, num_agents));
        }
    }
    
    info!("Directory tree creation complete");
    Ok(manifest)
}

// ============================================================================
// Path Selection for Directory-based Workloads
// ============================================================================

use crate::config::PathSelectionStrategy;

/// Path selector for directory-based workload operations
/// 
/// **IMPORTANT**: PathSelector is ONLY used when directory_structure is configured.
/// For simple mkdir/rmdir throughput testing without a tree, use random naming directly.
/// 
/// Implements 4 selection strategies that control contention level:
/// - Random: All agents pick any directory from tree (max contention)
/// - Partitioned: Agents prefer assigned dirs but can use others (medium contention)
/// - Exclusive: Agents only use assigned dirs (minimal contention)
/// - Weighted: Probabilistic mix based on partition_overlap
#[derive(Clone)]
pub struct PathSelector {
    /// Directory manifest with all paths (REQUIRED - PathSelector doesn't exist without tree)
    manifest: TreeManifest,
    
    /// This agent's ID (0-indexed)
    agent_id: usize,
    
    /// Total number of agents (used for validation)
    num_agents: usize,
    
    /// Path selection strategy
    strategy: PathSelectionStrategy,
    
    /// Overlap probability for weighted mode (0.0 = exclusive, 1.0 = random)
    partition_overlap: f64,
}

impl PathSelector {
    /// Create a new path selector for structured directory testing
    /// 
    /// # Arguments
    /// - `manifest`: TreeManifest with directory structure (REQUIRED)
    /// - `agent_id`: This agent's ID (0-indexed)
    /// - `num_agents`: Total number of agents
    /// - `strategy`: Path selection strategy
    /// - `partition_overlap`: Overlap probability for weighted mode
    pub fn new(
        manifest: TreeManifest,
        agent_id: usize,
        num_agents: usize,
        strategy: PathSelectionStrategy,
        partition_overlap: f64,
    ) -> Self {
        // Validate agent configuration
        if num_agents == 0 {
            warn!("PathSelector created with num_agents=0, setting to 1");
        }
        
        if agent_id >= num_agents && num_agents > 0 {
            warn!("PathSelector: agent_id ({}) >= num_agents ({}), path selection may not work correctly", 
                agent_id, num_agents);
        }
        
        Self {
            manifest,
            agent_id,
            num_agents,
            strategy,
            partition_overlap,
        }
    }
    
    /// Select a directory path based on the configured strategy
    /// 
    /// Always returns Some() since manifest is guaranteed to exist
    pub fn select_directory(&self) -> String {
        if self.manifest.all_directories.is_empty() {
            // Shouldn't happen with valid TreeManifest, but handle gracefully
            warn!("PathSelector has empty manifest - this indicates a bug");
            return "fallback_dir".to_string();
        }
        
        match self.strategy {
            PathSelectionStrategy::Random => self.select_random(),
            PathSelectionStrategy::Partitioned => self.select_partitioned(),
            PathSelectionStrategy::Exclusive => self.select_exclusive(),
            PathSelectionStrategy::Weighted => self.select_weighted(),
        }
    }
    
    /// Random: Pick any directory uniformly from the tree
    fn select_random(&self) -> String {
        use rand::{rng, Rng};
        
        let mut rng = rng();
        let idx = rng.random_range(0..self.manifest.all_directories.len());
        self.manifest.all_directories[idx].clone()
    }
    
    /// Partitioned: Prefer assigned directories, but can pick others
    /// Uses 70/30 split to reduce contention while allowing flexibility
    fn select_partitioned(&self) -> String {
        use rand::{rng, Rng};
        
        let mut rng = rng();
        
        // 70% chance to pick from assigned directories
        // 30% chance to pick from any directory
        if rng.random::<f64>() < 0.7 {
            // Pick from assigned directories
            let assigned = self.manifest.get_agent_dirs(self.agent_id);
            if !assigned.is_empty() {
                let idx = rng.random_range(0..assigned.len());
                return assigned[idx].clone();
            }
        }
        
        // Fall back to random selection
        self.select_random()
    }
    
    /// Exclusive: Only pick from assigned directories
    /// Minimal contention - each agent has its own namespace
    fn select_exclusive(&self) -> String {
        use rand::{rng, Rng};
        
        let assigned = self.manifest.get_agent_dirs(self.agent_id);
        
        if assigned.is_empty() {
            warn!("Agent {}/{} has no assigned directories in exclusive mode (total dirs: {}), falling back to random", 
                self.agent_id, self.num_agents, self.manifest.all_directories.len());
            return self.select_random();
        }
        
        let mut rng = rng();
        let idx = rng.random_range(0..assigned.len());
        assigned[idx].clone()
    }
    
    /// Weighted: Probabilistic mix based on partition_overlap
    /// - overlap=0.0: Exclusive (0% from other agents)
    /// - overlap=0.3: 30% from other agents, 70% from assigned
    /// - overlap=1.0: Random (100% from any directory)
    fn select_weighted(&self) -> String {
        use rand::{rng, Rng};
        
        let mut rng = rng();
        
        let use_assigned_probability = 1.0 - self.partition_overlap;
        
        if rng.random::<f64>() < use_assigned_probability {
            // Pick from assigned directories
            let assigned = self.manifest.get_agent_dirs(self.agent_id);
            if !assigned.is_empty() {
                let idx = rng.random_range(0..assigned.len());
                return assigned[idx].clone();
            }
        }
        
        // Pick from any directory (not assigned to this agent)
        // This creates controlled contention
        let all_dirs = &self.manifest.all_directories;
        let assigned_dirs = self.manifest.get_agent_dirs(self.agent_id);
        let assigned_set: std::collections::HashSet<_> = assigned_dirs.iter().collect();
        
        let other_dirs: Vec<_> = all_dirs.iter()
            .filter(|d| !assigned_set.contains(d))
            .collect();
        
        if !other_dirs.is_empty() {
            let idx = rng.random_range(0..other_dirs.len());
            other_dirs[idx].clone()
        } else {
            // No other dirs available, use assigned
            self.select_exclusive()
        }
    }
    
    /// Select a file path (directory + file) based on strategy
    /// 
    /// Returns full relative path: "d1_w1.dir/d2_w1.dir/d3_w1.dir/file_00001.dat"
    /// This is the primary method for GET/PUT/STAT/DELETE operations
    pub fn select_file(&self) -> String {
        use rand::{rng, Rng};
        
        // 1. Select directory using existing strategy
        let dir = self.select_directory_with_files();
        
        // 2. Pick a file within that directory using file ranges
        // Get file range for this directory
        if let Some((start_idx, end_idx)) = self.manifest.get_file_range(&dir) {
            if end_idx > start_idx {
                // Pick random file in range
                let mut rng = rng();
                let global_idx = rng.random_range(*start_idx..*end_idx);
                // Use get_file_path which returns directory + file based on global index
                if let Some(path) = self.manifest.get_file_path(global_idx) {
                    return path;
                }
            }
        }
        
        // Fallback (shouldn't happen with proper config)
        warn!("Directory {} has no files in manifest", dir);
        if dir.is_empty() {
            "fallback_file_00000.dat".to_string()
        } else {
            format!("{}/fallback_file_00000.dat", dir)
        }
    }
    
    /// Select a directory that has files (respects distribution strategy)
    /// Used when operation REQUIRES files (GET/STAT/DELETE)
    pub fn select_directory_with_files(&self) -> String {
        use rand::{rng, Rng};
        
        // Filter to directories that actually have files
        let dirs_with_files: Vec<&String> = self.manifest.file_ranges
            .iter()
            .map(|(dir, _)| dir)
            .collect();
        
        if dirs_with_files.is_empty() {
            warn!("No directories with files in manifest");
            return self.select_directory();
        }
        
        // Apply strategy to filtered list
        match self.strategy {
            PathSelectionStrategy::Random => {
                let mut rng = rng();
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
            PathSelectionStrategy::Exclusive => {
                // Pick from assigned directories that have files
                let assigned = self.manifest.get_agent_dirs(self.agent_id);
                let assigned_with_files: Vec<String> = assigned.into_iter()
                    .filter(|d| self.manifest.get_file_range(d).is_some())
                    .collect();
                    
                if assigned_with_files.is_empty() {
                    // Fallback to any directory with files
                    let mut rng = rng();
                    let idx = rng.random_range(0..dirs_with_files.len());
                    return dirs_with_files[idx].clone();
                }
                
                let mut rng = rng();
                let idx = rng.random_range(0..assigned_with_files.len());
                assigned_with_files[idx].clone()
            }
            PathSelectionStrategy::Partitioned => {
                let mut rng = rng();
                
                // 70% chance to pick from assigned directories with files
                if rng.random::<f64>() < 0.7 {
                    let assigned = self.manifest.get_agent_dirs(self.agent_id);
                    let assigned_with_files: Vec<String> = assigned.into_iter()
                        .filter(|d| self.manifest.get_file_range(d).is_some())
                        .collect();
                    
                    if !assigned_with_files.is_empty() {
                        let idx = rng.random_range(0..assigned_with_files.len());
                        return assigned_with_files[idx].clone();
                    }
                }
                
                // Fall back to random from all dirs with files
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
            PathSelectionStrategy::Weighted => {
                let mut rng = rng();
                let use_assigned_probability = 1.0 - self.partition_overlap;
                
                if rng.random::<f64>() < use_assigned_probability {
                    let assigned = self.manifest.get_agent_dirs(self.agent_id);
                    let assigned_with_files: Vec<String> = assigned.into_iter()
                        .filter(|d| self.manifest.get_file_range(d).is_some())
                        .collect();
                    
                    if !assigned_with_files.is_empty() {
                        let idx = rng.random_range(0..assigned_with_files.len());
                        return assigned_with_files[idx].clone();
                    }
                }
                
                // Pick from any directory with files
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
        }
    }
}

