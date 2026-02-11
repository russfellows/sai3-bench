// Configuration validation and summary display
// Used by both standalone binary (sai3-bench) and controller (sai3bench-ctl)

use crate::config::{Config, EnsureSpec, PageCacheMode, ProcessScaling};
use crate::directory_tree::{DirectoryStructureConfig, DirectoryTree, TreeManifest};
use crate::size_generator::SizeGenerator;
use crate::workload::BackendType;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use num_format::{Locale, ToFormattedString};
use std::time::Instant;

/// Format a u64 with thousand separators using system locale
/// Example: 64032768 → "64,032,768" (US locale)
fn format_with_thousands(n: u64) -> String {
    n.to_formatted_string(&Locale::en)
}

/// Format a usize with thousand separators
fn format_usize(n: usize) -> String {
    (n as u64).to_formatted_string(&Locale::en)
}

fn estimate_directory_tree_totals(dir_config: &DirectoryStructureConfig) -> Result<(u64, u64)> {
    if dir_config.width == 0 {
        anyhow::bail!("Directory width must be at least 1");
    }
    if dir_config.depth == 0 {
        anyhow::bail!("Directory depth must be at least 1");
    }

    let width = dir_config.width as u64;
    let depth = dir_config.depth as u64;
    let files_per_dir = dir_config.files_per_dir as u64;

    let mut total_dirs: u64 = 0;
    let mut dirs_at_level: u64 = 1;
    for _ in 0..depth {
        dirs_at_level = dirs_at_level
            .checked_mul(width)
            .ok_or_else(|| anyhow::anyhow!("Directory tree size overflow (width/depth too large)"))?;
        total_dirs = total_dirs
            .checked_add(dirs_at_level)
            .ok_or_else(|| anyhow::anyhow!("Directory tree size overflow (too many directories)"))?;
    }

    let total_files = match dir_config.distribution.as_str() {
        "bottom" => dirs_at_level
            .checked_mul(files_per_dir)
            .ok_or_else(|| anyhow::anyhow!("Directory tree size overflow (too many files)"))?,
        "all" => total_dirs
            .checked_mul(files_per_dir)
            .ok_or_else(|| anyhow::anyhow!("Directory tree size overflow (too many files)"))?,
        other => anyhow::bail!("Invalid distribution strategy: '{}'. Must be 'bottom' or 'all'", other),
    };

    Ok((total_dirs, total_files))
}

fn estimate_objects_bytes(ensure_objects: &[EnsureSpec]) -> Result<u64> {
    let mut total_bytes: u128 = 0;

    for spec in ensure_objects {
        let avg_bytes = if let Some(ref size_spec) = spec.size_spec {
            let mut generator = SizeGenerator::new(size_spec)?;
            generator.expected_mean() as u128
        } else if let (Some(min), Some(max)) = (spec.min_size, spec.max_size) {
            ((min + max) / 2) as u128
        } else {
            1024u128
        };

        let count = spec.count as u128;
        total_bytes = total_bytes
            .checked_add(count.saturating_mul(avg_bytes))
            .ok_or_else(|| anyhow::anyhow!("Estimated data size overflow (counts too large)"))?;
    }

    if total_bytes > u64::MAX as u128 {
        anyhow::bail!("Estimated data size exceeds u64::MAX");
    }

    Ok(total_bytes as u64)
}

fn read_rss_kb() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let value = rest.trim().split_whitespace().next()?;
            if let Ok(kb) = value.parse::<u64>() {
                return Some(kb);
            }
        }
    }
    None
}

pub fn apply_directory_tree_counts(config: &mut Config) -> Result<()> {
    let prepare = match config.prepare.as_mut() {
        Some(prepare) => prepare,
        None => return Ok(()),
    };

    let dir_config = match prepare.directory_structure.as_ref() {
        Some(dir_config) => dir_config,
        None => return Ok(()),
    };

    let (_total_dirs, total_files) = estimate_directory_tree_totals(dir_config)?;

    if prepare.ensure_objects.is_empty() {
        anyhow::bail!("directory_structure requires at least one ensure_objects entry");
    }

    for spec in prepare.ensure_objects.iter_mut() {
        if spec.count == 0 {
            spec.count = total_files;
        } else if spec.count != total_files {
            anyhow::bail!(
                "ensure_objects.count ({}) does not match directory tree total files ({}). Set count to {} or 0 to auto-calculate",
                spec.count,
                total_files,
                total_files
            );
        }
    }

    Ok(())
}

/// Display comprehensive configuration validation summary
/// This function is used by both the standalone binary and the controller
/// to provide consistent, detailed output for --dry-run mode
pub fn display_config_summary(config: &Config, config_path: &str) -> Result<()> {
    let mut config = config.clone();
    apply_directory_tree_counts(&mut config)?;

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
        
        // v0.8.51: Show timeout configuration if modified from defaults
        println!("│");
        println!("│ gRPC Timeouts:");
        if dist.grpc_keepalive_interval != 30 {
            println!("│   Keepalive:      {}s ⚠️  CUSTOM (default: 30s)", dist.grpc_keepalive_interval);
        }
        if dist.grpc_keepalive_timeout != 10 {
            println!("│   Keepalive TO:   {}s ⚠️  CUSTOM (default: 10s)", dist.grpc_keepalive_timeout);
        }
        if dist.agent_ready_timeout != 120 {
            let timeout_display = if dist.agent_ready_timeout >= 60 {
                format!("{}m", dist.agent_ready_timeout / 60)
            } else {
                format!("{}s", dist.agent_ready_timeout)
            };
            println!("│   Agent Ready:    {} ⚠️  CUSTOM (default: 120s)", timeout_display);
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
            
            // v0.8.61: Display concurrency configuration per agent
            let effective_concurrency = agent.concurrency_override.unwrap_or(config.concurrency);
            let num_endpoints = agent.multi_endpoint.as_ref()
                .map(|m| m.endpoints.len())
                .unwrap_or(1);
            
            if let Some(override_conc) = agent.concurrency_override {
                println!("│      Concurrency:     {} threads (OVERRIDE)", override_conc);
            } else {
                println!("│      Concurrency:     {} threads (global config)", effective_concurrency);
            }
            
            // v0.8.61: Critical validation - warn if concurrency < endpoints
            if effective_concurrency < num_endpoints {
                println!("│      ⚠️  WARNING: Only {:.1} threads per endpoint ({} threads / {} endpoints)",
                         effective_concurrency as f64 / num_endpoints as f64,
                         effective_concurrency, num_endpoints);
                println!("│      ⚠️  Some endpoints will be idle! Recommend concurrency >= {}", num_endpoints);
            } else {
                println!("│      Threads/Endpoint: {:.1} ({} threads / {} endpoints)",
                         effective_concurrency as f64 / num_endpoints as f64,
                         effective_concurrency, num_endpoints);
            }
            
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
        
        // v0.8.61: Show total concurrency summary
        let total_concurrency: usize = dist.agents.iter()
            .map(|a| a.concurrency_override.unwrap_or(config.concurrency))
            .sum();
        let total_endpoints: usize = dist.agents.iter()
            .map(|a| a.multi_endpoint.as_ref().map(|m| m.endpoints.len()).unwrap_or(1))
            .sum();
        
        println!("│");
        println!("│ TOTAL: {} threads across {} agents, {} endpoints total",
                 total_concurrency, dist.agents.len(), total_endpoints);
        println!("│        (Average: {:.1} threads per endpoint)",
                 total_concurrency as f64 / total_endpoints as f64);
        
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
        
        // Stage execution plan (v0.8.24+)
        if let Some(ref dist_config) = config.distributed {
            // Try to parse stages - if successful, show stage execution plan
            match dist_config.get_sorted_stages() {
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
                                    println!("│   Expected:   {} objects", format_usize(*count));
                                }
                            }
                            crate::config::StageSpecificConfig::Cleanup { expected_objects } => {
                                if let Some(count) = expected_objects {
                                    println!("│   Expected:   {} objects", format_usize(*count));
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
                            crate::config::StageSpecificConfig::Validation => {
                                if let Some(timeout) = stage.timeout_secs {
                                    println!("│   Timeout:    {}s", timeout);
                                }
                            }
                        }
                        
                        // Show barrier configuration with timeout details (v0.8.51)
                        if let Some(ref stage_barrier) = stage.barrier {
                            println!("│   Barrier:    ✅ {:?} (stage override)", stage_barrier.barrier_type);
                            
                            // Show timeout if different from default (120s)
                            if stage_barrier.agent_barrier_timeout != 120 {
                                let timeout_display = if stage_barrier.agent_barrier_timeout >= 86400 {
                                    format!("{}h", stage_barrier.agent_barrier_timeout / 3600)
                                } else if stage_barrier.agent_barrier_timeout >= 3600 {
                                    format!("{:.1}h", stage_barrier.agent_barrier_timeout as f64 / 3600.0)
                                } else if stage_barrier.agent_barrier_timeout >= 60 {
                                    format!("{}m", stage_barrier.agent_barrier_timeout / 60)
                                } else {
                                    format!("{}s", stage_barrier.agent_barrier_timeout)
                                };
                                println!("│   Timeout:    {} ⚠️  CUSTOM (default: 120s)", timeout_display);
                            }
                            
                            // Show heartbeat if different from default (30s)
                            if stage_barrier.heartbeat_interval != 30 {
                                println!("│   Heartbeat:  {}s ⚠️  CUSTOM (default: 30s)", 
                                    stage_barrier.heartbeat_interval);
                            }
                        } else if dist_config.barrier_sync.enabled {
                            println!("│   Barrier:    ✅ ENABLED (global config)");
                        } else {
                            println!("│   Barrier:    ❌ DISABLED");
                        }
                        
                        if idx < stages.len() - 1 {
                            println!("│");
                        }
                    }
                    
                    println!("│");
                    
                    // Check if any barriers are actually enabled
                    let has_any_barriers = dist_config.barrier_sync.enabled || 
                        stages.iter().any(|s| s.barrier.is_some());
                    
                    if has_any_barriers {
                        println!("│ ✅ Agents will synchronize at barriers between stages");
                    } else {
                        println!("│ ⚠️  WARNING: No barriers configured!");
                        println!("│    Agents will NOT wait for each other between stages");
                        println!("│    This may cause race conditions in distributed workloads");
                        println!("│    Consider adding barrier configuration (see docs)");
                    }
                    
                    println!("└──────────────────────────────────────────────────────────────────────┘");
                    println!();
                    
                    // v0.8.52: Check for conflicting cleanup configuration
                    // If explicit stages exist + has cleanup stage + prepare.cleanup=false → WARN
                    if let Some(ref prepare) = config.prepare {
                        let has_cleanup_stage = stages.iter().any(|s| matches!(s.config, crate::config::StageSpecificConfig::Cleanup { .. }));
                        
                        if has_cleanup_stage && !prepare.cleanup {
                            println!("┌─ ⚠️  CONFIGURATION CONFLICT DETECTED ⚠️  ────────────────────────────┐");
                            println!("│                                                                      │");
                            println!("│ 🔴 CONFLICTING CLEANUP SETTINGS:                                     │");
                            println!("│                                                                      │");
                            println!("│   prepare.cleanup: false     (requests: KEEP objects)                │");
                            println!("│   stages: includes CLEANUP   (requests: DELETE objects)              │");
                            println!("│                                                                      │");
                            println!("│ 🚨 PRECEDENCE DECISION:                                              │");
                            println!("│                                                                      │");
                            println!("│   ✅ Explicit YAML stages take precedence                            │");
                            println!("│   ❌ prepare.cleanup: false is IGNORED                               │");
                            println!("│                                                                      │");
                            println!("│ 📢 WHAT WILL HAPPEN:                                                 │");
                            println!("│                                                                      │");
                            println!("│   → Cleanup stage WILL execute                                       │");
                            println!("│   → All {} objects WILL be deleted                                   │", 
                                if let Some(ref dir_struct) = prepare.directory_structure {
                                    match estimate_directory_tree_totals(dir_struct) {
                                        Ok((_dirs, total_files)) => format_with_thousands(total_files),
                                        Err(_) => "prepared".to_string(),
                                    }
                                } else {
                                    "prepared".to_string()
                                });
                            println!("│   → Data will NOT be kept for reuse                                 │");
                            println!("│                                                                      │");
                            println!("│ 🔧 TO FIX THIS CONFLICT:                                             │");
                            println!("│                                                                      │");
                            println!("│   Option 1 (Keep data):                                              │");
                            println!("│     Remove the cleanup stage from 'stages:' list                     │");
                            println!("│                                                                      │");
                            println!("│   Option 2 (Delete data):                                            │");
                            println!("│     Set prepare.cleanup: true to match stages intent                 │");
                            println!("│                                                                      │");
                            println!("└──────────────────────────────────────────────────────────────────────┘");
                            println!();
                        } else if !has_cleanup_stage && prepare.cleanup {
                            println!("┌─ ⚠️  CONFIGURATION CONFLICT DETECTED ⚠️  ────────────────────────────┐");
                            println!("│                                                                      │");
                            println!("│ 🔴 CONFLICTING CLEANUP SETTINGS:                                     │");
                            println!("│                                                                      │");
                            println!("│   prepare.cleanup: true      (requests: DELETE objects)              │");
                            println!("│   stages: NO cleanup stage   (requests: KEEP objects)                │");
                            println!("│                                                                      │");
                            println!("│ 🚨 PRECEDENCE DECISION:                                              │");
                            println!("│                                                                      │");
                            println!("│   ✅ Explicit YAML stages take precedence                            │");
                            println!("│   ❌ prepare.cleanup: true is IGNORED                                │");
                            println!("│                                                                      │");
                            println!("│ 📢 WHAT WILL HAPPEN:                                                 │");
                            println!("│                                                                      │");
                            println!("│   → NO cleanup stage will execute                                    │");
                            println!("│   → All {} objects WILL be kept                                      │", 
                                if let Some(ref dir_struct) = prepare.directory_structure {
                                    match estimate_directory_tree_totals(dir_struct) {
                                        Ok((_dirs, total_files)) => format_with_thousands(total_files),
                                        Err(_) => "prepared".to_string(),
                                    }
                                } else {
                                    "prepared".to_string()
                                });
                            println!("│   → Data available for subsequent runs                               │");
                            println!("│                                                                      │");
                            println!("│ 🔧 TO FIX THIS CONFLICT:                                             │");
                            println!("│                                                                      │");
                            println!("│   Option 1 (Keep data):                                              │");
                            println!("│     Set prepare.cleanup: false to match stages intent                │");
                            println!("│                                                                      │");
                            println!("│   Option 2 (Delete data):                                            │");
                            println!("│     Add cleanup stage to 'stages:' list                              │");
                            println!("│                                                                      │");
                            println!("└──────────────────────────────────────────────────────────────────────┘");
                            println!();
                        }
                    }
                    
                    // v0.8.51: Display prepare configuration when using stages
                    // (since it's hidden in the main prepare section for stage-driven workflows)
                    if let Some(ref prepare) = config.prepare {
                        println!("┌─ Prepare Phase Configuration (for prepare stage) ───────────────────┐");
                        println!("│ Strategy:           {:?}", prepare.prepare_strategy);
                        println!("│ Skip Verification:  {} {}", 
                            if prepare.skip_verification { "✅ YES" } else { "❌ NO" },
                            if prepare.skip_verification { "(no LIST before create)" } else { "(LIST to check existing)" });
                        println!("│ Force Overwrite:    {}", 
                            if prepare.force_overwrite { "✅ YES (overwrite existing)" } else { "❌ NO (skip existing)" });
                        println!("│ Cleanup:            {}", 
                            if prepare.cleanup { "✅ YES (delete after test)" } else { "❌ NO (keep objects)" });
                        
                        // Show barrier timeout from prepare stage if available
                        if let Some(ref dist_config) = config.distributed {
                            if let Ok(stages) = dist_config.get_sorted_stages() {
                                if let Some(prepare_stage) = stages.iter().find(|s| matches!(s.config, crate::config::StageSpecificConfig::Prepare { .. })) {
                                    if let Some(ref barrier) = prepare_stage.barrier {
                                        let timeout_secs = barrier.agent_barrier_timeout;
                                        let timeout_display = if timeout_secs >= 86400 {
                                            format!("{} hours", timeout_secs / 3600)
                                        } else if timeout_secs >= 3600 {
                                            format!("{:.1} hours", timeout_secs as f64 / 3600.0)
                                        } else if timeout_secs >= 60 {
                                            format!("{} minutes", timeout_secs / 60)
                                        } else {
                                            format!("{} seconds", timeout_secs)
                                        };
                                        println!("│ Max Duration:       {} (barrier timeout)", timeout_display);
                                    }
                                }
                            }
                        }
                        
                        // Show directory tree summary if configured
                        if let Some(ref dir_config) = prepare.directory_structure {
                            println!("│");
                            println!("│ 📁 Directory Tree:");
                            let leaf_dirs = (dir_config.width as u64).pow(dir_config.depth as u32);
                            println!("│   Width × Depth:    {} × {} = {} leaf dirs", 
                                dir_config.width, dir_config.depth, format_with_thousands(leaf_dirs));
                            println!("│   Files/Dir:        {} files per leaf", format_usize(dir_config.files_per_dir));
                            let (total_dirs, total_files) = estimate_directory_tree_totals(dir_config)?;
                            println!("│   Total Dirs:       {} directories", format_with_thousands(total_dirs));
                            println!("│   Total Files:      {} files", format_with_thousands(total_files));

                            let total_bytes = estimate_objects_bytes(&prepare.ensure_objects)?;
                            let (size_val, size_unit) = format_bytes(total_bytes);
                            println!("│   Total Data Size:  {} bytes ({:.2} {})",
                                format_with_thousands(total_bytes), size_val, size_unit);
                        }
                        
                        println!("└──────────────────────────────────────────────────────────────────────┘");
                        println!();
                    }
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
        .and_then(|d| d.get_sorted_stages().ok())
        .map(|stages| !stages.is_empty())
        .unwrap_or(false);
    
    if let Some(ref prepare) = config.prepare {
        if !using_stages {
            println!("┌─ Prepare Phase ──────────────────────────────────────────────────────┐");
            println!("│ Objects will be created BEFORE test execution");
            println!("│");
            println!("│ Strategy:           {:?}", prepare.prepare_strategy);
            println!("│ Skip Verification:  {} {}", 
                if prepare.skip_verification { "✅ YES" } else { "❌ NO" },
                if prepare.skip_verification { "(no LIST before create)" } else { "(LIST to check existing)" });
            println!("│ Force Overwrite:    {}", 
                if prepare.force_overwrite { "✅ YES (overwrite existing)" } else { "❌ NO (skip existing)" });
            println!("│");
        
        // Directory tree structure (if configured)
        if let Some(ref dir_config) = prepare.directory_structure {
            println!("│ 📁 Directory Tree Structure:");
            println!("│   Width:            {} subdirectories per level", dir_config.width);
            println!("│   Depth:            {} levels", dir_config.depth);
            println!("│   Files/Dir:        {} files per directory", dir_config.files_per_dir);
            println!("│   Distribution:     {} ({}", dir_config.distribution,
                if dir_config.distribution == "bottom" { "files only in leaf directories" } 
                else { "files at every level" });
            println!("│   Directory Mask:   {}", dir_config.dir_mask);
            println!("│");

            let (total_dirs, total_files) = estimate_directory_tree_totals(dir_config)?;
            println!("│ 📊 Calculated Tree Metrics:");
            println!("│   Total Directories:  {}", format_with_thousands(total_dirs));
            println!("│   Total Files:        {}", format_with_thousands(total_files));

            let total_bytes = estimate_objects_bytes(&prepare.ensure_objects)?;
            let (size_val, size_unit) = format_bytes(total_bytes);
            println!("│   Total Data Size:    {} bytes ({:.2} {})", 
                format_with_thousands(total_bytes), size_val, size_unit);
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
            println!("│   Count:            {} objects", format_with_thousands(spec.count));
            
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

        if prepare.directory_structure.is_none() {
            let total_bytes = estimate_objects_bytes(&prepare.ensure_objects)?;
            let (size_val, size_unit) = format_bytes(total_bytes);
            println!("│");
            println!("│ Estimated Total Data Size: {} bytes ({:.2} {})",
                format_with_thousands(total_bytes), size_val, size_unit);
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
    
    // v0.8.60: KV Cache Configuration (always show when prepare is enabled)
    if config.prepare.is_some() {
        println!("┌─ KV Cache & Checkpointing ───────────────────────────────────────────┐");
        
        // Checkpoint interval (always show, including default)
        let checkpoint_interval = config.cache_checkpoint_interval_secs;
        if checkpoint_interval > 0 {
            let (interval_val, interval_unit) = if checkpoint_interval >= 3600 {
                (checkpoint_interval / 3600, "hours")
            } else if checkpoint_interval >= 60 {
                (checkpoint_interval / 60, "minutes")
            } else {
                (checkpoint_interval, "seconds")
            };
            
            let interval_display = if checkpoint_interval == 300 {
                format!("{} {} (DEFAULT)", interval_val, interval_unit)
            } else {
                format!("{} {} ⚠️  CUSTOM", interval_val, interval_unit)
            };
            
            println!("│ Checkpoint Interval: {}", interval_display);
            println!("│   ✅ Periodic checkpointing ENABLED");
            println!("│   📦 Creates tar.zst archives during prepare");
            println!("│   🔄 Maximum data loss: {} {}", interval_val, interval_unit);
            println!("│   💾 Archive format: .sai3-cache-agent-{{id}}.tar.zst");
        } else {
            println!("│ Checkpoint Interval: DISABLED (0 seconds)");
            println!("│   ⚠️  Only final checkpoint at end of prepare");
            println!("│   🔴 Risk: ALL metadata lost if prepare crashes");
        }
        println!("│");
        
        // KV cache directory (show where LSM operations will be isolated)
        if let Some(ref dist) = config.distributed {
            // Distributed mode - check for global or per-agent overrides
            if let Some(ref kv_cache_dir) = dist.kv_cache_dir {
                println!("│ KV Cache Directory:  {} (GLOBAL)", kv_cache_dir.display());
            } else {
                println!("│ KV Cache Directory:  /tmp/ (DEFAULT - system temp)");
            }
            
            // Check for per-agent overrides
            let has_agent_overrides = dist.agents.iter()
                .any(|a| a.kv_cache_dir.is_some());
            
            if has_agent_overrides {
                println!("│");
                println!("│ Per-Agent Overrides:");
                for (idx, agent) in dist.agents.iter().enumerate() {
                    if let Some(ref agent_kv_dir) = agent.kv_cache_dir {
                        let agent_id = agent.id.as_deref().unwrap_or("auto");
                        println!("│   Agent {}: {} (id: {})", idx + 1, agent_kv_dir.display(), agent_id);
                    }
                }
            }
        } else {
            // Standalone mode - no distributed config
            println!("│ KV Cache Directory:  /tmp/ (DEFAULT - system temp)");
        }
        
        println!("│");
        println!("│ 📝 Purpose: Isolate LSM I/O from storage under test");
        println!("│ 🎯 Benefits: Accurate performance measurements");
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // Workload operations
    println!("┌─ Workload Operations ────────────────────────────────────────────────┐");
    let total_weight: u32 = config.workload.iter().map(|w| w.weight).sum();
    println!("│ {} operation types, total weight: {}", config.workload.len(), total_weight);
    println!("│ Execution Duration: {:?}", config.duration);
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

    // Dry-run sample generation (always executed)
    println!("┌─ Dry-Run Sample Generation ──────────────────────────────────────────┐");
    match config.prepare.as_ref() {
        Some(prepare) if !prepare.ensure_objects.is_empty() => {
            let sample_limit = crate::constants::DRY_RUN_SAMPLE_SIZE as u64;
            let total_objects = if let Some(ref dir_config) = prepare.directory_structure {
                estimate_directory_tree_totals(dir_config)?.1
            } else {
                prepare.ensure_objects.iter().map(|s| s.count).sum()
            };
            let sample_count = std::cmp::min(sample_limit, total_objects) as usize;

            if sample_count == 0 {
                println!("│ Sample:       0 objects (nothing to generate)");
            } else {
                let spec = &prepare.ensure_objects[0];
                let (endpoints, base_uri_label) = if spec.use_multi_endpoint {
                    if let Some(ref multi) = config.multi_endpoint {
                        (multi.endpoints.clone(), "global multi-endpoint")
                    } else if let Some(ref dist) = config.distributed {
                        let first_agent = dist.agents.first().and_then(|a| a.multi_endpoint.as_ref());
                        if let Some(agent_multi) = first_agent {
                            (agent_multi.endpoints.clone(), "agent-1 multi-endpoint")
                        } else {
                            let base_uri = spec.get_base_uri(None)?;
                            (vec![base_uri], "base_uri")
                        }
                    } else {
                        let base_uri = spec.get_base_uri(None)?;
                        (vec![base_uri], "base_uri")
                    }
                } else {
                    let base_uri = spec.get_base_uri(None)?;
                    (vec![base_uri], "base_uri")
                };

                let base_uri = endpoints.first().cloned().unwrap_or_else(|| "unknown://".to_string());
                let seed = base_uri.as_bytes().iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
                let size_spec = spec.get_size_spec();
                let mut size_generator = SizeGenerator::new_with_seed(&size_spec, seed)?;

                let rss_before = read_rss_kb();
                let start = Instant::now();

                let pb = ProgressBar::new(sample_count as u64);
                pb.set_style(ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects {msg}"
                )?);
                pb.set_message("generating sample");

                let mut sample: Vec<(String, u64)> = Vec::with_capacity(sample_count);

                let manifest = if let Some(ref dir_config) = prepare.directory_structure {
                    let tree = DirectoryTree::new(dir_config.clone())?;
                    Some(TreeManifest::from_tree(&tree))
                } else {
                    None
                };

                for idx in 0..sample_count {
                    let size = size_generator.generate();
                    let endpoint = &endpoints[idx % endpoints.len()];
                    let uri = if let Some(ref tree_manifest) = manifest {
                        if let Some(rel_path) = tree_manifest.get_file_path(idx) {
                            if endpoint.ends_with('/') {
                                format!("{}{}", endpoint, rel_path)
                            } else {
                                format!("{}/{}", endpoint, rel_path)
                            }
                        } else {
                            let key = format!("prepared-{:08}.dat", idx);
                            if endpoint.ends_with('/') {
                                format!("{}{}", endpoint, key)
                            } else {
                                format!("{}/{}", endpoint, key)
                            }
                        }
                    } else {
                        let key = format!("prepared-{:08}.dat", idx);
                        if endpoint.ends_with('/') {
                            format!("{}{}", endpoint, key)
                        } else {
                            format!("{}/{}", endpoint, key)
                        }
                    };

                    sample.push((uri, size));
                    pb.inc(1);
                }

                pb.finish_with_message("sample generated");

                let elapsed = start.elapsed();
                let rss_after = read_rss_kb();

                println!("│ Sample:       {} objects (first {} of {})", format_with_thousands(sample_count as u64), sample_count, format_with_thousands(total_objects));
                println!("│ Source:       {}", base_uri_label);
                println!("│ Time:         {:.2}s", elapsed.as_secs_f64());
                match (rss_before, rss_after) {
                    (Some(before), Some(after)) => {
                        let delta_kb = after.saturating_sub(before);
                        let (size_val, size_unit) = format_bytes(delta_kb * 1024);
                        println!("│ RSS Delta:    {} KiB ({:.2} {})", format_with_thousands(delta_kb), size_val, size_unit);
                    }
                    _ => {
                        println!("│ RSS Delta:    unavailable (/proc/self/status)");
                    }
                }
            }
        }
        _ => {
            println!("│ Sample:       skipped (no prepare.ensure_objects configured)");
        }
    }
    println!("└──────────────────────────────────────────────────────────────────────┘");
    println!();
    
    // v0.8.53: Multi-endpoint + directory tree validation with sample file paths
    if let Some(ref prepare) = config.prepare {
        if let Some(ref dir_config) = prepare.directory_structure {
            // Check if any operation uses multi_endpoint
            let has_multi_endpoint_ops = prepare.ensure_objects.iter().any(|spec| spec.use_multi_endpoint);
            let workload_uses_multi_ep = config.workload.iter().any(|wo| {
                matches!(&wo.spec, 
                    crate::config::OpSpec::Get { use_multi_endpoint: true, .. } | 
                    crate::config::OpSpec::Put { use_multi_endpoint: true, .. } |
                    crate::config::OpSpec::Stat { use_multi_endpoint: true, .. } |
                    crate::config::OpSpec::Delete { use_multi_endpoint: true, .. })
            });
            
            if has_multi_endpoint_ops || workload_uses_multi_ep {
                println!("┌─ Multi-Endpoint File Distribution ──────────────────────────────────┐");
                
                // Collect ALL endpoints from ALL agents (distributed mode) or global config
                // Structure: Vec<(agent_id, endpoint_url)>
                let mut all_endpoints: Vec<(String, String)> = Vec::new();
                
                if let Some(ref dist) = config.distributed {
                    // Distributed mode: collect from each agent
                    for (idx, agent) in dist.agents.iter().enumerate() {
                        if let Some(ref me_cfg) = agent.multi_endpoint {
                            let agent_id = agent.id.clone().unwrap_or_else(|| format!("agent-{}", idx));
                            for endpoint in &me_cfg.endpoints {
                                all_endpoints.push((agent_id.clone(), endpoint.clone()));
                            }
                        }
                    }
                    
                    // Fallback to global config if no per-agent endpoints
                    if all_endpoints.is_empty() {
                        if let Some(ref me_cfg) = config.multi_endpoint {
                            for endpoint in &me_cfg.endpoints {
                                all_endpoints.push(("global".to_string(), endpoint.clone()));
                            }
                        }
                    }
                } else if let Some(ref me_cfg) = config.multi_endpoint {
                    // Single-node mode: use global config
                    for endpoint in &me_cfg.endpoints {
                        all_endpoints.push(("single".to_string(), endpoint.clone()));
                    }
                }
                
                if !all_endpoints.is_empty() {
                    println!("│ ⚠️  IMPORTANT: Files are distributed ROUND-ROBIN by index");
                    println!("│");
                    println!("│ Total endpoints: {} across {} agent(s)", 
                        all_endpoints.len(),
                        if let Some(ref dist) = config.distributed { dist.agents.len() } else { 1 }
                    );
                    println!("│ Distribution pattern: file_NNNNNNNN.dat → endpoint[N % {}]", all_endpoints.len());
                    
                    // Show pattern description
                    if all_endpoints.len() == 2 {
                        println!("│   • Even indices (0, 2, 4, 6...) → endpoint 1");
                        println!("│   • Odd indices (1, 3, 5, 7...) → endpoint 2");
                    } else if all_endpoints.len() == 4 {
                        println!("│   • Indices 0, 4, 8, 12... → endpoint 1");
                        println!("│   • Indices 1, 5, 9, 13... → endpoint 2");
                        println!("│   • Indices 2, 6, 10, 14... → endpoint 3");
                        println!("│   • Indices 3, 7, 11, 15... → endpoint 4");
                    } else {
                        for i in 0..std::cmp::min(all_endpoints.len(), 6) {
                            println!("│   • Indices {}, {}, {}... → endpoint {}", 
                                i, i + all_endpoints.len(), i + 2*all_endpoints.len(), i + 1);
                        }
                        if all_endpoints.len() > 6 {
                            println!("│   ... ({} more endpoints)", all_endpoints.len() - 6);
                        }
                    }
                    println!("│");
                    
                    // Generate sample file paths using DirectoryTree
                    use crate::directory_tree::DirectoryTree;
                    match DirectoryTree::new(dir_config.clone()) {
                        Ok(tree) => {
                            use crate::directory_tree::TreeManifest;
                            let manifest = TreeManifest::from_tree(&tree);
                            
                            // Show 2 sample files per endpoint, grouped by agent
                            println!("│ Sample files (first 2 per endpoint):");
                            println!("│");
                            
                            let mut current_agent = String::new();
                            for (ep_idx, (agent_id, endpoint)) in all_endpoints.iter().enumerate() {
                                // Print agent header when we switch agents
                                if agent_id != &current_agent {
                                    if !current_agent.is_empty() {
                                        println!("│");
                                    }
                                    current_agent = agent_id.clone();
                                    if let Some(ref dist) = config.distributed {
                                        if dist.agents.len() > 1 {
                                            println!("│ Agent: {}", agent_id);
                                        }
                                    }
                                }
                                
                                println!("│ Endpoint {} ({}):", ep_idx + 1, endpoint);
                                
                                // Find first 2 files that go to this endpoint
                                let mut files_shown = 0;
                                for i in 0..manifest.total_files {
                                    if i % all_endpoints.len() == ep_idx {
                                        if let Some(rel_path) = manifest.get_file_path(i) {
                                            // Build full URI
                                            let full_uri = if endpoint.ends_with('/') {
                                                format!("{}{}", endpoint, rel_path)
                                            } else {
                                                format!("{}/{}", endpoint, rel_path)
                                            };
                                            println!("│   {}", full_uri);
                                            files_shown += 1;
                                            if files_shown >= 2 {
                                                break;
                                            }
                                        }
                                    }
                                }
                                
                                // Count total files for this endpoint
                                let total_on_endpoint = (manifest.total_files + all_endpoints.len() - 1 - ep_idx) / all_endpoints.len();
                                if total_on_endpoint > files_shown {
                                    println!("│   ... ({} more on this endpoint)", total_on_endpoint - files_shown);
                                }
                            }
                        }
                        Err(e) => {
                            println!("│ ⚠️  Could not generate sample paths: {}", e);
                        }
                    }
                }
                
                println!("└──────────────────────────────────────────────────────────────────────┘");
                println!();
            }
        }
    }
    
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
