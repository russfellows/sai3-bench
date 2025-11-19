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
        println!("│");
        println!("│ Agent List:");
        for (idx, agent) in dist.agents.iter().enumerate() {
            let id = agent.id.as_deref().unwrap_or("auto");
            println!("│   {}: {} (id: {})", idx + 1, agent.address, id);
        }
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // Prepare configuration
    if let Some(ref prepare) = config.prepare {
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
                let avg_bytes = if let Some(ref obj_spec) = prepare.ensure_objects.first() {
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
            
            println!("│ Flat Objects Section {}:", idx + 1);
            println!("│   URI:              {}", spec.base_uri);
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
            if matches!(spec.fill, crate::config::FillPattern::Random) {
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
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // Workload operations
    println!("┌─ Workload Operations ────────────────────────────────────────────────┐");
    let total_weight: u32 = config.workload.iter().map(|w| w.weight).sum();
    println!("│ {} operation types, total weight: {}", config.workload.len(), total_weight);
    println!("│");
    
    for (idx, weighted_op) in config.workload.iter().enumerate() {
        let percentage = (weighted_op.weight as f64 / total_weight as f64) * 100.0;
        
        let (op_name, details) = match &weighted_op.spec {
            crate::config::OpSpec::Get { path } => {
                ("GET", format!("path: {}", path))
            },
            crate::config::OpSpec::Put { path, object_size, size_spec, dedup_factor, compress_factor } => {
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
            crate::config::OpSpec::List { path } => {
                ("LIST", format!("path: {}", path))
            },
            crate::config::OpSpec::Stat { path } => {
                ("STAT", format!("path: {}", path))
            },
            crate::config::OpSpec::Delete { path } => {
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
