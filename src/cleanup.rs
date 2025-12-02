//! Cleanup operations for sai3-bench
//! 
//! This module provides functionality for cleaning up prepared objects after workload execution.
//! Supports both modes:
//! - Normal cleanup: Delete objects created during prepare phase (uses prepared object list)
//! - Cleanup-only: List and delete objects independently (for resuming interrupted cleanup)

use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::{PrepareConfig, CleanupMode};
use crate::prepare::{PreparedObject, create_store_for_uri};
use crate::directory_tree::TreeManifest;

/// List existing objects without creating any new ones
/// 
/// This is a simplified version of prepare_objects that ONLY lists existing objects
/// and returns them. It does NOT create missing objects or try to reach any target count.
/// 
/// Use this for cleanup-only mode with skip_verification=false, where you want to
/// discover what files actually exist and clean them up, without creating anything new.
/// 
/// # Arguments
/// 
/// * `config` - Prepare configuration (uses base_uri and directory_structure)
/// * `agent_id` - This agent's index (0-based)
/// * `num_agents` - Total number of agents
/// 
/// # Returns
/// 
/// Returns a tuple of (objects, tree_manifest) where:
/// - objects: List of PreparedObject entries for existing files (all have created=false)
/// - tree_manifest: Optional tree structure info if directory_structure is configured
pub async fn list_existing_objects(
    config: &PrepareConfig,
    agent_id: usize,
    num_agents: usize,
) -> Result<(Vec<PreparedObject>, Option<TreeManifest>)> {
    info!("Listing existing objects for agent {}/{}", agent_id, num_agents);
    
    // Get base URI from config
    let base_uri = config.ensure_objects.first()
        .map(|spec| spec.base_uri.as_str())
        .ok_or_else(|| anyhow!("ensure_objects must have at least one entry"))?;
    
    // Check if we're in directory tree mode
    let is_tree_mode = config.directory_structure.is_some();
    if is_tree_mode {
        info!("Directory tree mode detected - will list recursively");
    }
    
    let store = create_store_for_uri(base_uri)?;
    
    // List all existing objects
    let list_base = if base_uri.ends_with('/') {
        base_uri.to_string()
    } else {
        format!("{}/", base_uri)
    };
    
    info!("Listing objects from: {}", list_base);
    let all_files = store.list(&list_base, true).await
        .context("Failed to list existing objects")?;
    
    info!("Found {} total files in storage", all_files.len());
    
    // Parse filenames to extract indices and filter by agent responsibility
    let mut prepared_objects = Vec::new();
    
    if is_tree_mode {
        // Directory tree mode: parse file_NNNNNNNN.dat filenames
        // NOTE: Include ALL files - cleanup_prepared_objects will distribute work via modulo
        for path in &all_files {
            if let Some(filename) = path.rsplit('/').next() {
                if let Some(idx_str) = filename.strip_prefix("file_").and_then(|s| s.strip_suffix(".dat")) {
                    if let Ok(_idx) = idx_str.parse::<u64>() {
                        // List returns full URIs (file://..., s3://..., etc)
                        // Use path directly if it's already a full URI, otherwise prepend list_base
                        let full_uri = if path.contains("://") {
                            path.clone()
                        } else {
                            format!("{}{}", list_base, path)
                        };
                        
                        prepared_objects.push(PreparedObject {
                            uri: full_uri,
                            size: 0,  // Size unknown from listing
                            created: false,  // All objects already existed
                        });
                    }
                }
            }
        }
    } else {
        // Flat mode: include all files
        // NOTE: No filtering here - cleanup_prepared_objects will distribute work via modulo
        // Sort for deterministic ordering
        let mut sorted_files = all_files.clone();
        sorted_files.sort();
        
        for path in sorted_files.iter() {
            // List returns full URIs (file://..., s3://..., etc)
            // Use path directly if it's already a full URI, otherwise prepend list_base
            let full_uri = if path.contains("://") {
                path.clone()
            } else {
                format!("{}{}", list_base, path)
            };
            
            prepared_objects.push(PreparedObject {
                uri: full_uri,
                size: 0,
                created: false,
            });
        }
    }
    
    info!("Agent {}/{}: Found {} objects to cleanup (filtered from {} total)", 
          agent_id, num_agents, prepared_objects.len(), all_files.len());
    
    // For cleanup listing, we don't need to return a manifest
    Ok((prepared_objects, None))
}

/// Cleanup prepared objects after workload completion
/// 
/// Deletes objects in parallel with configurable error handling mode.
/// Records DELETE operations as META in LiveStatsTracker if provided.
/// 
/// # Arguments
/// 
/// * `objects` - List of objects to delete
/// * `tree_manifest` - Optional tree structure (for rmdir cleanup)
/// * `agent_id` - This agent's index for distributed cleanup
/// * `num_agents` - Total number of agents
/// * `cleanup_mode` - Error handling strategy (strict/tolerant/best-effort)
/// * `live_stats_tracker` - Optional tracker for recording DELETE latencies as META operations
pub async fn cleanup_prepared_objects(
    objects: &[PreparedObject],
    _tree_manifest: Option<&TreeManifest>,
    agent_id: usize,
    num_agents: usize,
    cleanup_mode: CleanupMode,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<()> {
    if objects.is_empty() {
        info!("No objects to clean up");
        return Ok(());
    }
    
    info!("Agent {}/{}: Cleaning up {} objects with {} workers", 
          agent_id, num_agents, objects.len(), 32);
    
    // Determine which objects this agent should clean (distributed cleanup)
    let my_objects: Vec<_> = if num_agents > 1 {
        // Each agent handles objects whose index % num_agents == agent_id
        objects.iter()
            .enumerate()
            .filter(|(idx, _)| idx % num_agents == agent_id)
            .map(|(_, obj)| obj.clone())
            .collect()
    } else {
        objects.iter().cloned().collect()
    };
    
    let my_objects_count = my_objects.len();
    info!("Agent {}/{}: Responsible for {} of {} objects", 
          agent_id, num_agents, my_objects_count, objects.len());
    
    if my_objects.is_empty() {
        info!("Agent {}/{}: No objects to delete (all assigned to other agents)", agent_id, num_agents);
        return Ok(());
    }
    
    // v0.8.9: Create store ONCE using first object's URI (all objects share same backend)
    // This fixes a major performance issue where create_store_for_uri was called per-object
    let first_uri = &my_objects[0].uri;
    let store = Arc::new(create_store_for_uri(first_uri)
        .context("Failed to create object store for cleanup")?);
    info!("Agent {}/{}: Created object store for cleanup", agent_id, num_agents);
    
    // Delete objects in parallel
    use futures::stream::{self, StreamExt};
    
    let results: Vec<_> = stream::iter(my_objects)
        .map(|obj| {
            let tracker = live_stats_tracker.clone();
            let store = store.clone();
            async move {
                let start = std::time::Instant::now();
                
                // Delete using full URI (same as prepare.rs cleanup)
                match store.delete(&obj.uri).await {
                    Ok(_) => {
                        // Record successful DELETE as META operation
                        if let Some(ref t) = tracker {
                            t.record_meta(start.elapsed());
                            // v0.8.9: Increment stage progress for cleanup
                            t.increment_stage_progress();
                        }
                        Ok(DeleteResult::Success)
                    }
                    Err(e) => {
                        let err_msg = e.to_string().to_lowercase();
                        if err_msg.contains("not found") || err_msg.contains("no such") || err_msg.contains("404") {
                            // Object already deleted (tolerable in most modes)
                            // v0.8.9: Increment stage progress even for already-deleted
                            if let Some(ref t) = tracker {
                                t.increment_stage_progress();
                            }
                            match cleanup_mode {
                                CleanupMode::Strict => Err(anyhow!("Object not found (strict mode): {}", obj.uri)),
                                CleanupMode::Tolerant | CleanupMode::BestEffort => {
                                    debug!("Object already deleted: {}", obj.uri);
                                    Ok(DeleteResult::AlreadyDeleted)
                                }
                            }
                        } else {
                            // Other error
                            match cleanup_mode {
                                CleanupMode::Strict | CleanupMode::Tolerant => {
                                    Err(anyhow!("Failed to delete {}: {}", obj.uri, e))
                                }
                                CleanupMode::BestEffort => {
                                    warn!("Failed to delete {} (continuing): {}", obj.uri, e);
                                    Ok(DeleteResult::Error)
                                }
                            }
                        }
                    }
                }
            }
        })
        .buffer_unordered(32)
        .collect()
        .await;
    
    // Summarize results
    let mut succeeded = 0;
    let mut already_deleted = 0;
    let mut errors = 0;
    
    for result in results {
        match result {
            Ok(DeleteResult::Success) => succeeded += 1,
            Ok(DeleteResult::AlreadyDeleted) => already_deleted += 1,
            Ok(DeleteResult::Error) => errors += 1,
            Err(e) => {
                errors += 1;
                // Note: error! macro removed to fix unused import
                // Use eprintln! instead
                eprintln!("Cleanup error: {}", e);
                if cleanup_mode == CleanupMode::Strict {
                    return Err(e);
                }
            }
        }
    }
    
    info!("Agent {}/{}: deleted {} objects ({} succeeded, {} already deleted, {} errors)", 
          agent_id, num_agents, my_objects_count, succeeded, already_deleted, errors);
    
    // Note: Directory cleanup for tree structures is handled separately
    // via rmdir operations after all files are deleted
    
    Ok(())
}

enum DeleteResult {
    Success,
    AlreadyDeleted,
    Error,
}
