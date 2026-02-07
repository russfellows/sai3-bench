//! Cleanup and verification operations
//!
//! Handles deletion of prepared objects and verification of object creation.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::config::PrepareConfig;
use crate::workload::create_store_for_uri;
use super::metrics::PreparedObject;

pub async fn cleanup_prepared_objects(
    objects: &[PreparedObject],
    tree_manifest: Option<&crate::directory_tree::TreeManifest>,
    agent_id: usize,
    num_agents: usize,
    cleanup_mode: crate::config::CleanupMode,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<()> {
    if objects.is_empty() {
        return Ok(());
    }
    
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::collections::HashSet;
    
    // Filter to objects this agent should handle
    let my_objects: Vec<_> = if let Some(manifest) = tree_manifest {
        // Tree mode: use deterministic file assignment
        let my_paths: HashSet<String> = manifest
            .get_agent_file_paths(agent_id, num_agents)
            .into_iter()
            .collect();
        
        objects.iter()
            .filter(|obj| obj.created)
            .filter(|obj| {
                // Extract relative path from URI for comparison
                // URI format: "scheme://host/path/to/file.dat"
                // Need to extract: "path/to/file.dat" portion
                if let Some(pos) = obj.uri.rfind('/') {
                    let filename = &obj.uri[pos+1..];
                    // Check if this file is in our assigned paths
                    my_paths.iter().any(|p| p.ends_with(filename))
                } else {
                    false
                }
            })
            .collect()
    } else {
        // Flat mode: distribute by file index (parsed from URI)
        // URI format: "file:///path/to/prepared-00000042.dat" or "deletable-00000042.dat"
        objects.iter()
            .filter(|obj| {
                if !obj.created {
                    return false;
                }
                
                if num_agents <= 1 {
                    return true;
                }
                
                // Extract file index from URI
                // Look for pattern: "prepared-NNNNNNNN.dat" or "deletable-NNNNNNNN.dat"
                if let Some(filename_start) = obj.uri.rfind('/') {
                    let filename = &obj.uri[filename_start + 1..];
                    // Parse index from "prepared-00000042.dat" or "deletable-00000042.dat"
                    if let Some(dash_pos) = filename.find('-') {
                        if let Some(dot_pos) = filename.find('.') {
                            let index_str = &filename[dash_pos + 1..dot_pos];
                            if let Ok(file_index) = index_str.parse::<usize>() {
                                return file_index % num_agents == agent_id;
                            }
                        }
                    }
                }
                
                // Fallback: if we can't parse index, don't delete
                tracing::warn!("Could not parse file index from URI: {}", obj.uri);
                false
            })
            .collect()
    };
    
    if my_objects.is_empty() {
        if num_agents > 1 {
            info!("Agent {}/{}: No objects to clean up", agent_id + 1, num_agents);
        } else {
            info!("No objects to clean up");
        }
        return Ok(());
    }
    
    let delete_count = my_objects.len();
    
    // TODO: Accept concurrency parameter from caller (for now, use reasonable default)
    // This function is called during cleanup and doesn't have access to config
    let concurrency = 32;
    
    if num_agents > 1 {
        info!("Agent {}/{}: Cleaning up {} objects with {} workers", 
            agent_id + 1, num_agents, delete_count, concurrency);
    } else {
        info!("Cleaning up {} prepared objects with {} workers", delete_count, concurrency);
    }
    
    // Create progress bar for cleanup
    let pb = ProgressBar::new(delete_count as u64);
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({per_sec}) {msg}"
    )?);
    
    let msg = if num_agents > 1 {
        format!("agent {}/{} cleaning with {} workers", agent_id + 1, num_agents, concurrency)
    } else {
        format!("cleaning with {} workers", concurrency)
    };
    pb.set_message(msg);
    
    // Track error statistics
    let error_count = Arc::new(AtomicU64::new(0));
    let not_found_count = Arc::new(AtomicU64::new(0));
    let success_count = Arc::new(AtomicU64::new(0));
    
    // Execute DELETE operations in parallel with semaphore-controlled concurrency
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    let pb_clone = pb.clone();
    
    for obj in my_objects {
        let sem2 = sem.clone();
        let pb2 = pb_clone.clone();
        let uri = obj.uri.clone();
        let mode = cleanup_mode;
        let errors = error_count.clone();
        let not_founds = not_found_count.clone();
        let successes = success_count.clone();
        let tracker = live_stats_tracker.clone();
        
        futs.push(tokio::spawn(async move {
            let _permit = sem2.acquire_owned().await.unwrap();
            
            // Start timing for latency tracking
            let start = std::time::Instant::now();
            
            // Create store and delete object
            match create_store_for_uri(&uri) {
                Ok(store) => {
                    match store.delete(&uri).await {
                        Ok(_) => {
                            successes.fetch_add(1, Ordering::Relaxed);
                            // Record successful DELETE as META operation
                            if let Some(ref t) = tracker {
                                t.record_meta(start.elapsed());
                            }
                        }
                        Err(e) => {
                            let err_str = e.to_string().to_lowercase();
                            let is_not_found = err_str.contains("not found") 
                                || err_str.contains("404") 
                                || err_str.contains("nosuchkey")
                                || err_str.contains("does not exist");
                            
                            if is_not_found {
                                not_founds.fetch_add(1, Ordering::Relaxed);
                                // Record "not found" DELETE as META operation (tolerant/best-effort mode)
                                if let Some(ref t) = tracker {
                                    t.record_meta(start.elapsed());
                                }
                                match mode {
                                    crate::config::CleanupMode::Strict => {
                                        tracing::warn!("Object not found (strict mode): {}", uri);
                                        errors.fetch_add(1, Ordering::Relaxed);
                                    }
                                    crate::config::CleanupMode::Tolerant | crate::config::CleanupMode::BestEffort => {
                                        tracing::debug!("Object already deleted: {}", uri);
                                    }
                                }
                            } else {
                                errors.fetch_add(1, Ordering::Relaxed);
                                // Record failed DELETE as META operation
                                if let Some(ref t) = tracker {
                                    t.record_meta(start.elapsed());
                                }
                                match mode {
                                    crate::config::CleanupMode::BestEffort => {
                                        tracing::warn!("Failed to delete {} (best-effort, continuing): {}", uri, e);
                                    }
                                    _ => {
                                        tracing::warn!("Failed to delete {}: {}", uri, e);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    // Record store creation failure as META operation
                    if let Some(ref t) = tracker {
                        t.record_meta(start.elapsed());
                    }
                    tracing::warn!("Failed to create store for {}: {}", uri, e);
                }
            }
            
            pb2.inc(1);
        }));
    }
    
    // Wait for all deletion tasks to complete
    // Tasks don't return errors (they log warnings instead), so we just need to detect panics
    let mut yield_counter = 0u64;  // v0.8.51: Counter for periodic yields
    while let Some(res) = futs.next().await {
        // v0.8.51: Yield every 100 operations to prevent executor starvation
        yield_counter += 1;
        if yield_counter.is_multiple_of(100) {
            tokio::task::yield_now().await;
        }
        
        if let Err(e) = res {
            tracing::error!("Cleanup task panicked: {}", e);
            // Continue with remaining tasks even if one panicked
        }
    }
    
    let final_errors = error_count.load(Ordering::Relaxed);
    let final_not_found = not_found_count.load(Ordering::Relaxed);
    let final_success = success_count.load(Ordering::Relaxed);
    
    let summary = format!(
        "deleted {} objects ({} succeeded, {} already deleted, {} errors)",
        delete_count, final_success, final_not_found, final_errors
    );
    pb.finish_with_message(summary.clone());
    
    // Log final statistics
    if num_agents > 1 {
        info!("Agent {}/{}: {}", agent_id + 1, num_agents, summary);
    } else {
        info!("{}", summary);
    }
    
    // Return error only in strict mode with actual errors
    if cleanup_mode == crate::config::CleanupMode::Strict && final_errors > 0 {
        anyhow::bail!("Cleanup failed with {} errors (strict mode)", final_errors);
    }
    
    Ok(())
}

/// Generate list of objects for cleanup-only mode (v0.8.7+)
/// 
/// Creates PreparedObject list based on config WITHOUT listing existing objects.
/// Uses same deterministic algorithm as prepare phase (modulo distribution).
/// 
/// This function is used when:
/// - cleanup_only mode (duration=0, workload=[], cleanup=true)
/// - skip_verification=true (do NOT list existing objects)
/// 
/// Each agent generates ONLY the objects it would have created:
/// - Agent 0: indices 0, 2, 4, 6, ... (i % num_agents == 0)
/// - Agent 1: indices 1, 3, 5, 7, ... (i % num_agents == 1)
pub fn generate_cleanup_objects(
    config: &PrepareConfig,
    agent_id: usize,
    num_agents: usize,
) -> Result<Vec<PreparedObject>> {
    let mut objects = Vec::new();
    
    for spec in &config.ensure_objects {
        // Get effective base_uri for cleanup
        // Note: In cleanup, we don't have multi_endpoint_config, so just use the base_uri or fail gracefully
        let base_uri = spec.get_base_uri(None)
            .unwrap_or_else(|_| {
                // Fallback: try to extract from environment or use a default
                warn!("Failed to determine base_uri for cleanup - skipping spec");
                String::new()
            });
        
        if base_uri.is_empty() {
            continue;
        }
        
        // Determine which pool(s) to clean
        // For simplicity, cleanup both prepared and deletable if they exist
        let pools = vec!["prepared", "deletable"];
        
        for prefix in pools {
            for i in 0..spec.count {
                let index = i as usize;
                
                // Distributed mode: only handle objects for this agent
                if num_agents > 1 && index % num_agents != agent_id {
                    continue;
                }
                
                // Generate URI using same naming convention as prepare
                let key = format!("{}-{:08}.dat", prefix, i);
                let uri = if base_uri.ends_with('/') {
                    format!("{}{}", base_uri, key)
                } else {
                    format!("{}/{}", base_uri, key)
                };
                
                // Mark as created=true so cleanup will process them
                objects.push(PreparedObject {
                    uri,
                    size: 0,  // Size doesn't matter for cleanup
                    created: true,
                });
            }
        }
    }
    
    if num_agents > 1 {
        info!("Agent {}/{}: Generated {} objects for cleanup (skip_verification=true)", 
              agent_id + 1, num_agents, objects.len());
    } else {
        info!("Generated {} objects for cleanup (skip_verification=true)", objects.len());
    }
    
    Ok(objects)
}

/// Verify that prepared objects exist and are accessible
pub async fn verify_prepared_objects(config: &PrepareConfig) -> Result<()> {
    info!("Starting verification of prepared objects");
    
    for spec in &config.ensure_objects {
        // Get effective base_uri for verification
        let base_uri = spec.get_base_uri(None)
            .context("Failed to determine base_uri for verification")?;
        
        let store = create_store_for_uri(&base_uri)?;
        
        // List existing objects
        info!("Verifying objects at {}", base_uri);
        let existing = store.list(&base_uri, true).await
            .context("Failed to list objects during verification")?;
        
        let found_count = existing.len();
        let expected_count = spec.count as usize;
        
        if found_count < expected_count {
            anyhow::bail!("Verification failed: Found {} objects but expected {} at {}",
                  found_count, expected_count, base_uri);
        }
        
        info!("Found {}/{} objects, verifying accessibility...", found_count, expected_count);
        
        // Verify accessibility by attempting to stat each object
        let pb = ProgressBar::new(expected_count as u64);
        pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} verified {msg}"
        )?);
        
        let mut accessible_count = 0;
        let mut inaccessible = Vec::new();
        
        for uri in existing.iter().take(expected_count) {
            // Try to stat the object using the same store
            match store.stat(uri).await {
                Ok(_metadata) => {
                    accessible_count += 1;
                    pb.inc(1);
                }
                Err(e) => {
                    inaccessible.push(format!("{}: {}", uri, e));
                    pb.inc(1);
                }
            }
        }
        
        pb.finish_and_clear();
        
        if !inaccessible.is_empty() {
            eprintln!("\nInaccessible objects:");
            for issue in &inaccessible {
                eprintln!("  ✗ {}", issue);
            }
            anyhow::bail!("Verification failed: {}/{} objects accessible at {}",
                  accessible_count, expected_count, base_uri);
        }
        
        println!("✓ {}/{} objects verified and accessible at {}", 
                 accessible_count, expected_count, base_uri);
        info!("Verification successful: {}/{} objects", accessible_count, expected_count);
    }
    
    Ok(())
}

