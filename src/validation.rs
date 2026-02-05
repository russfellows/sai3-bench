// Configuration validation and summary display
// Used by both standalone binary (sai3-bench) and controller (sai3bench-ctl)

use crate::config::{Config, PageCacheMode, ProcessScaling};
use crate::size_generator::SizeGenerator;
use crate::workload::BackendType;
use anyhow::{Context, Result};

/// Display comprehensive configuration validation summary
/// This function is used by both the standalone binary and the controller
/// to provide consistent, detailed output for --dry-run mode
pub fn display_config_summary(config: &Config, config_path: &str) -> Result<()> {
    println!("╔═══════════════════════════════════════════════════════════════════════╗");
    println!("║           CONFIGURATION VALIDATION & TEST SUMMARY                    ║");
    println!("╚═══════════════════════════════════════════════════════════════════════╝");
    println!();
    println!("✅ Config file parsed successfully: {}", config_path);
    println!();
    
    // Basic configuration
    println!("┌─ Test Configuration ─────────────────────────────────────────────────┐");
    println!("│ Duration:     {:?}", config.duration);
    println!("│ Concurrency:  {} threads", config.concurrency);
    
    // Multi-process scaling (v0.7.3+)
    if let Some(ref processes) = config.processes {
        let resolved = processes.resolve();
        match processes {
            ProcessScaling::Single => {
                println!("│ Processes:    1 (single process mode)");
            },
            ProcessScaling::Auto => {
                println!("│ Processes:    {} (auto-detected physical cores)", resolved);
            },
            ProcessScaling::Manual(n) => {
                println!("│ Processes:    {} (manual configuration)", n);
            },
        }
        println!("│ Total Workers: {} (processes × threads)", resolved * config.concurrency);
    }
    
    if let Some(ref target) = config.target {
        let backend = BackendType::from_uri(target);
        println!("│ Target URI:   {}", target);
        println!("│ Backend:      {}", backend.name());
    } else {
        println!("│ Target URI:   (not set - using absolute URIs in operations)");
    }
    
    // Performance logging status
    if let Some(ref perf_log) = config.perf_log {
        println!("│ Perf Log:     ✅ ENABLED (interval: {:?})", perf_log.interval);
        if let Some(ref path) = perf_log.path {
            println!("│               Output: {}", path.display());
        } else {
            println!("│               Output: ./results/perf_log.tsv (default)");
        }
    } else {
        println!("│ Perf Log:     ❌ DISABLED");
    }
    
    println!("└──────────────────────────────────────────────────────────────────────┘");
    println!();
    
    // RangeEngine configuration
    if let Some(ref range_config) = config.range_engine {
        println!("┌─ RangeEngine Configuration ──────────────────────────────────────────┐");
        println!("│ Enabled:      {}", if range_config.enabled { "✅ YES" } else { "❌ NO" });
        if range_config.enabled {
            let min_mb = range_config.min_split_size / (1024 * 1024);
            let chunk_mb = range_config.chunk_size / (1024 * 1024);
            println!("│ Min Size:     {} MiB (files >= this use concurrent range downloads)", min_mb);
            println!("│ Chunk Size:   {} MiB per range request", chunk_mb);
            println!("│ Max Ranges:   {} concurrent ranges per file", range_config.max_concurrent_ranges);
        }
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // PageCache configuration
    if let Some(page_cache_mode) = config.page_cache_mode {
        println!("┌─ Page Cache Configuration (file:// and direct:// only) ─────────────┐");
        let mode_str = match page_cache_mode {
            PageCacheMode::Auto => "Auto (Sequential for large files, Random for small)",
            PageCacheMode::Sequential => "Sequential (streaming workloads)",
            PageCacheMode::Random => "Random (random access patterns)",
            PageCacheMode::DontNeed => "DontNeed (read-once data, free immediately)",
            PageCacheMode::Normal => "Normal (default kernel heuristics)",
        };
        println!("│ Mode:         {:?} - {}", page_cache_mode, mode_str);
        println!("│ Note:         Linux/Unix only, uses posix_fadvise() hints");
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // Multi-endpoint configuration for standalone mode (v0.8.22+)
    if config.multi_endpoint.is_some() && config.distributed.is_none() {
        if let Some(ref multi) = config.multi_endpoint {
            println!("┌─ Multi-Endpoint Configuration ───────────────────────────────────────┐");
            println!("│ Strategy:     {}", multi.strategy);
            println!("│ Endpoints:    {} total", multi.endpoints.len());
            for (idx, endpoint) in multi.endpoints.iter().enumerate() {
                println!("│   {}: {}", idx + 1, endpoint);
            }
            println!("└──────────────────────────────────────────────────────────────────────┘");
            println!();
        }
    }
    
    // Distributed configuration (v0.7.5+)
    if let Some(ref dist) = config.distributed {
        println!("┌─ Distributed Configuration ──────────────────────────────────────────┐");
        println!("│ Agents:           {}", dist.agents.len());
        println!("│ Shared Filesystem: {}", dist.shared_filesystem);
        println!("│ Tree Creation:    {:?}", dist.tree_creation_mode);
        println!("│ Path Selection:   {:?}", dist.path_selection);
        if matches!(dist.path_selection, crate::config::PathSelectionStrategy::Partitioned | crate::config::PathSelectionStrategy::Weighted) {
            println!("│ Partition Overlap: {:.1}%", dist.partition_overlap * 100.0);
        }
        
        // v0.8.22: Display global multi-endpoint configuration if present
        if let Some(ref global_multi) = config.multi_endpoint {
            println!("│");
            println!("│ Global Multi-Endpoint Configuration:");
            println!("│   Strategy:       {}", global_multi.strategy);
            println!("│   Endpoints:      {} total", global_multi.endpoints.len());
            for (idx, endpoint) in global_multi.endpoints.iter().enumerate() {
                println!("│     {}: {}", idx + 1, endpoint);
            }
            println!("│   (applies to agents without per-agent override)");
        }
        
        println!("│");
        println!("│ Agent List:");
        for (idx, agent) in dist.agents.iter().enumerate() {
            let id = agent.id.as_deref().unwrap_or("auto");
            println!("│   {}: {} (id: {})", idx + 1, agent.address, id);
            
            // v0.8.22: Display per-agent multi-endpoint configuration if present
            if let Some(ref agent_multi) = agent.multi_endpoint {
                println!("│      Multi-Endpoint:  {} strategy", agent_multi.strategy);
                println!("│      Endpoints:       {} total", agent_multi.endpoints.len());
                for (ep_idx, endpoint) in agent_multi.endpoints.iter().enumerate() {
                    println!("│        {}: {}", ep_idx + 1, endpoint);
                }
            } else if config.multi_endpoint.is_some() {
                println!("│      Multi-Endpoint:  (using global configuration)");
            }
            
            if idx < dist.agents.len() - 1 {
                println!("│");
            }
        }
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
        
        // Stage execution plan (v0.8.24+)
        if let Some(ref dist_config) = config.distributed {
            // Try to parse stages - if successful, show stage execution plan
            match dist_config.get_sorted_stages(config.duration) {
                Ok(stages) if !stages.is_empty() => {
                    println!("┌─ YAML-Driven Stage Execution Plan ──────────────────────────────────┐");
                    println!("│ {} stages will execute in order:", stages.len());
                    println!("│");
                    
                    for (idx, stage) in stages.iter().enumerate() {
                        let stage_num = idx + 1;
                        let stage_type = match &stage.config {
                            crate::config::StageSpecificConfig::Prepare { .. } => "PREPARE",
                            crate::config::StageSpecificConfig::Execute { .. } => "EXECUTE",
                            crate::config::StageSpecificConfig::Cleanup { .. } => "CLEANUP",
                            crate::config::StageSpecificConfig::Custom { .. } => "CUSTOM",
                            crate::config::StageSpecificConfig::Hybrid { .. } => "HYBRID",
                            crate::config::StageSpecificConfig::Validation { .. } => "VALIDATION",
                        };
                        
                        let completion = match stage.completion {
                            crate::config::CompletionCriteria::Duration => "Duration",
                            crate::config::CompletionCriteria::TasksDone => "TasksDone",
                            crate::config::CompletionCriteria::ScriptExit => "ScriptExit",
                            crate::config::CompletionCriteria::ValidationPassed => "ValidationPassed",
                            crate::config::CompletionCriteria::DurationOrTasks => "DurationOrTasks",
                        };
                        
                        println!("│ Stage {}: {} (order: {})", stage_num, stage.name, stage.order);
                        println!("│   Type:       {}", stage_type);
                        println!("│   Completion: {}", completion);
                        
                        // Show type-specific details
                        match &stage.config {
                            crate::config::StageSpecificConfig::Execute { duration } => {
                                println!("│   Duration:   {:?}", duration);
                            }
                            crate::config::StageSpecificConfig::Prepare { expected_objects } => {
                                if let Some(count) = expected_objects {
                                    println!("│   Expected:   {} objects", count);
                                }
                            }
                            crate::config::StageSpecificConfig::Cleanup { expected_objects } => {
                                if let Some(count) = expected_objects {
                                    println!("│   Expected:   {} objects", count);
                                }
                            }
                            crate::config::StageSpecificConfig::Custom { command, args } => {
                                println!("│   Command:    {} {:?}", command, args);
                            }
                            crate::config::StageSpecificConfig::Hybrid { max_duration, expected_tasks } => {
                                if let Some(duration) = max_duration {
                                    println!("│   Max Dur:    {:?}", duration);
                                }
                                if let Some(tasks) = expected_tasks {
                                    println!("│   Tasks:      {}", tasks);
                                }
                            }
                            crate::config::StageSpecificConfig::Validation { timeout_secs } => {
                                if let Some(timeout) = timeout_secs {
                                    println!("│   Timeout:    {}s", timeout);
                                }
                            }
                        }
                        
                        // Show barrier configuration
                        if let Some(ref stage_barrier) = stage.barrier {
                            println!("│   Barrier:    ✅ {:?} (override)", stage_barrier.barrier_type);
                        } else if dist_config.barrier_sync.enabled {
                            println!("│   Barrier:    ✅ ENABLED (global)");
                        } else {
                            println!("│   Barrier:    ❌ DISABLED");
                        }
                        
                        if idx < stages.len() - 1 {
                            println!("│");
                        }
                    }
                    
                    println!("│");
                    println!("│ All agents will synchronize at barriers between stages");
                    println!("└──────────────────────────────────────────────────────────────────────┘");
                    println!();
                }
                Ok(_) => {
                    // No stages configured - using deprecated prepare/execute/cleanup flow
                    println!("┌─ Execution Plan (DEPRECATED) ────────────────────────────────────────┐");
                    println!("│ Using hardcoded prepare → execute → cleanup flow");
                    println!("│");
                    println!("│ ⚠️  RECOMMENDATION: Migrate to YAML-driven stages");
                    println!("│    Add 'stages:' section to distributed config for better control");
                    println!("└──────────────────────────────────────────────────────────────────────┘");
                    println!();
                }
                Err(e) => {
                    // Stage parsing error - show error
                    println!("┌─ Stage Configuration ERROR ──────────────────────────────────────────┐");
                    println!("│ ❌ Failed to parse stages: {}", e);
                    println!("│");
                    println!("│ The distributed test will NOT run with this configuration.");
                    println!("│ Fix the stage configuration and run --dry-run again.");
                    println!("└──────────────────────────────────────────────────────────────────────┘");
                    println!();
                    return Err(anyhow::anyhow!("Stage configuration error: {}", e));
                }
            }
        }
    }
    
    // Prepare configuration
    // Skip this section if using YAML-driven stages (prepare behavior defined in stages)
    let using_stages = config.distributed.as_ref()
        .and_then(|d| d.get_sorted_stages(config.duration).ok())
        .map(|stages| !stages.is_empty())
        .unwrap_or(false);
    
    if let Some(ref prepare) = config.prepare {
        if !using_stages {
            println!("┌─ Prepare Phase ──────────────────────────────────────────────────────┐");
            println!("│ Objects will be created BEFORE test execution");
            println!("│");
        
        // Directory tree structure (if configured)
        if let Some(ref dir_config) = prepare.directory_structure {
            use crate::directory_tree::{DirectoryTree, TreeManifest};
            
            println!("│ 📁 Directory Tree Structure:");
            println!("│   Width:            {} subdirectories per level", dir_config.width);
            println!("│   Depth:            {} levels", dir_config.depth);
            println!("│   Files/Dir:        {} files per directory", dir_config.files_per_dir);
            println!("│   Distribution:     {} ({}", dir_config.distribution,
                if dir_config.distribution == "bottom" { "files only in leaf directories" } 
                else { "files at every level" });
            println!("│   Directory Mask:   {}", dir_config.dir_mask);
            println!("│");
            
            // Calculate totals using DirectoryTree
            let tree = DirectoryTree::new(dir_config.clone())
                .context("Failed to create directory tree for dry-run analysis")?;
            let manifest = TreeManifest::from_tree(&tree);
            
            println!("│ 📊 Calculated Tree Metrics:");
            println!("│   Total Directories:  {}", manifest.total_dirs);
            println!("│   Total Files:        {}", manifest.total_files);
            
            // Calculate total data size
            let total_bytes = if manifest.total_files > 0 {
                // Use file size spec from ensure_objects if available
                let avg_bytes = if let Some(obj_spec) = prepare.ensure_objects.first() {
                    if let Some(ref size_spec) = obj_spec.size_spec {
                        let mut generator = SizeGenerator::new(size_spec)?;
                        generator.expected_mean()
                    } else if let (Some(min), Some(max)) = (obj_spec.min_size, obj_spec.max_size) {
                        (min + max) / 2
                    } else {
                        1024 // Default 1KB
                    }
                } else {
                    1024 // Default 1KB
                };
                
                manifest.total_files as u64 * avg_bytes
            } else {
                0
            };
            
            // Format bytes in human-readable format
            let (size_val, size_unit) = format_bytes(total_bytes);
            
            println!("│   Total Data Size:    {} bytes ({:.2} {})", 
                total_bytes, size_val, size_unit);
            println!("│");
        }
        
        for (idx, spec) in prepare.ensure_objects.iter().enumerate() {
            if prepare.directory_structure.is_some() && spec.count == 0 {
                // Skip showing flat object sections when using directory tree and count is 0
                continue;
            }
            
            // Determine URI display based on configuration mode
            let uri_display = if spec.base_uri.is_none() && spec.use_multi_endpoint {
                // In distributed mode with multi_endpoint, each agent uses its own endpoints
                if config.distributed.is_some() {
                    String::from("Per-agent multi-endpoint (see agent configs)")
                } else {
                    // Standalone mode with multi_endpoint
                    String::from("Multi-endpoint load balancing (see above)")
                }
            } else {
                spec.get_base_uri(None)
                    .unwrap_or_else(|_| String::from("<not configured>"))
            };
            
            println!("│ Flat Objects Section {}:", idx + 1);
            println!("│   URI:              {}", uri_display);
            println!("│   Count:            {} objects", spec.count);
            
            // Display size information
            if let Some(ref size_spec) = spec.size_spec {
                let mut generator = SizeGenerator::new(size_spec)?;
                println!("│   Size:             {}", generator.description());
            } else if let (Some(min), Some(max)) = (spec.min_size, spec.max_size) {
                if min == max {
                    println!("│   Size:             {} bytes (fixed)", min);
                } else {
                    println!("│   Size:             {} - {} bytes (uniform)", min, max);
                }
            }
            
            // Display fill pattern
            println!("│   Fill Pattern:     {:?}", spec.fill);
            if matches!(spec.fill, crate::config::FillPattern::Random | crate::config::FillPattern::Prand) {
                let dedup_desc = if spec.dedup_factor == 1 { 
                    "all unique".to_string() 
                } else { 
                    format!("{:.1}% dedup", (spec.dedup_factor - 1) as f64 / spec.dedup_factor as f64 * 100.0) 
                };
                println!("│   Dedup Factor:     {} ({})", spec.dedup_factor, dedup_desc);
                
                let compress_desc = if spec.compress_factor == 1 { 
                    "uncompressible".to_string() 
                } else { 
                    format!("{:.1}% compressible", (spec.compress_factor - 1) as f64 / spec.compress_factor as f64 * 100.0) 
                };
                println!("│   Compress Factor:  {} ({})", spec.compress_factor, compress_desc);
            }
            
            if idx < prepare.ensure_objects.len() - 1 {
                println!("│");
            }
        }
        
        println!("│");
        println!("│ Cleanup:            {}", if prepare.cleanup { "✅ YES (delete after test)" } else { "❌ NO (keep objects)" });
        if prepare.post_prepare_delay > 0 {
            println!("│ Post-Prepare Delay: {}s (eventual consistency wait)", prepare.post_prepare_delay);
        }
        
        // v0.8.19: Warn if skip_verification=true and workload has GET operations
        if prepare.skip_verification {
            // Check if any workload operation is a GET
            let has_get_ops = config.workload.iter().any(|w| matches!(w.spec, crate::config::OpSpec::Get { .. }));
            if has_get_ops {
                println!("│");
                println!("│ ⚠️  WARNING: skip_verification=true but workload has GET operations");
                println!("│     Objects will NOT be created during prepare phase!");
                println!("│     GET operations will fail unless objects exist from a previous run.");
                println!("│");
                println!("│     To fix: Set skip_verification=false (default) to create objects");
            }
        }
        
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
        } // End of !using_stages check
    }
    
    // Workload operations
    println!("┌─ Workload Operations ────────────────────────────────────────────────┐");
    let total_weight: u32 = config.workload.iter().map(|w| w.weight).sum();
    println!("│ {} operation types, total weight: {}", config.workload.len(), total_weight);
    println!("│");
    
    for (idx, weighted_op) in config.workload.iter().enumerate() {
        let percentage = (weighted_op.weight as f64 / total_weight as f64) * 100.0;
        
        let (op_name, details) = match &weighted_op.spec {
            crate::config::OpSpec::Get { path, .. } => {
                ("GET", format!("path: {}", path))
            },
            crate::config::OpSpec::Put { path, object_size, size_spec, dedup_factor, compress_factor, .. } => {
                let mut details = format!("path: {}", path);
                if let Some(ref spec) = size_spec {
                    let mut generator = SizeGenerator::new(spec)?;
                    details.push_str(&format!(", size: {}", generator.description()));
                } else if let Some(size) = object_size {
                    details.push_str(&format!(", size: {} bytes", size));
                }
                details.push_str(&format!(", dedup: {}, compress: {}", dedup_factor, compress_factor));
                ("PUT", details)
            },
            crate::config::OpSpec::List { path, .. } => {
                ("LIST", format!("path: {}", path))
            },
            crate::config::OpSpec::Stat { path, .. } => {
                ("STAT", format!("path: {}", path))
            },
            crate::config::OpSpec::Delete { path, .. } => {
                ("DELETE", format!("path: {}", path))
            },
            crate::config::OpSpec::Mkdir { path } => {
                ("MKDIR", format!("path: {}", path))
            },
            crate::config::OpSpec::Rmdir { path, recursive } => {
                let rec = if *recursive { " (recursive)" } else { "" };
                ("RMDIR", format!("path: {}{}", path, rec))
            },
        };
        
        println!("│ Op {}: {} - {:.1}% (weight: {})", idx + 1, op_name, percentage, weighted_op.weight);
        println!("│       {}", details);
        
        if let Some(concurrency) = weighted_op.concurrency {
            println!("│       concurrency override: {} threads", concurrency);
        }
        
        if idx < config.workload.len() - 1 {
            println!("│");
        }
    }
    
    println!("└──────────────────────────────────────────────────────────────────────┘");
    println!();
    
    // Summary
    println!("✅ Configuration is valid and ready to run");
    println!();
    
    // Show appropriate command to execute
    if config.distributed.is_some() {
        println!("To execute this distributed test, run:");
        println!("  sai3bench-ctl --agents <agent1>,<agent2>,... run --config {}", config_path);
    } else {
        println!("To execute this test, run:");
        println!("  sai3-bench run --config {}", config_path);
    }
    println!();
    
    Ok(())
}

/// Format bytes into human-readable format (TiB, GiB, MiB, KiB, B)
fn format_bytes(bytes: u64) -> (f64, &'static str) {
    if bytes >= 1024 * 1024 * 1024 * 1024 {
        (bytes as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0), "TiB")
    } else if bytes >= 1024 * 1024 * 1024 {
        (bytes as f64 / (1024.0 * 1024.0 * 1024.0), "GiB")
    } else if bytes >= 1024 * 1024 {
        (bytes as f64 / (1024.0 * 1024.0), "MiB")
    } else if bytes >= 1024 {
        (bytes as f64 / 1024.0, "KiB")
    } else {
        (bytes as f64, "B")
    }
}
