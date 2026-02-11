// src/config_converter.rs
//! Legacy YAML config converter to explicit stages format
//!
//! Converts old implicit-stage configs (top-level duration/workload) to new
//! explicit-stage format (distributed.stages array) required by v0.8.61+.
//!
//! Conversion rules:
//! 1. Always create preflight validation stage (order: 1)
//! 2. If prepare section exists → create prepare stage (order: 2)
//! 3. Always create execute stage from workload+duration (order: next)
//! 4. If prepare.cleanup is true → create cleanup stage (order: last)

use crate::config::{
    CompletionCriteria, Config, DistributedConfig, StageConfig, StageSpecificConfig,
};
use anyhow::{Context, Result};
use std::path::Path;

/// Check if a config has explicit stages defined
pub fn has_stages(config: &Config) -> bool {
    config
        .distributed
        .as_ref()
        .map(|d| !d.stages.is_empty())
        .unwrap_or(false)
}

/// Check if a config needs conversion to explicit stages format
///
/// Returns true if:
/// - Has workload section (the main indicator of an executable config)
/// - Does NOT have stages section (or it's empty)
/// 
/// Note: Does NOT require distributed section - standalone configs also need stages in v0.8.61+
pub fn needs_conversion(config: &Config) -> bool {
    // Must have workload to convert (all executable configs have this)
    if config.workload.is_empty() {
        return false;
    }

    // Must NOT already have stages
    !has_stages(config)
}

/// Generate stages section from legacy config
///
/// Creates up to 4 stages:
/// 1. Preflight validation (always)
/// 2. Prepare (if prepare section exists)
/// 3. Execute workload (always)
/// 4. Cleanup (if prepare.cleanup is true)
pub fn generate_stages(config: &Config) -> Result<Vec<StageConfig>> {
    let mut stages = Vec::new();
    let mut order = 1;

    // Stage 1: Preflight validation (always first)
    stages.push(StageConfig {
        name: "preflight".to_string(),
        order,
        completion: CompletionCriteria::ValidationPassed,
        barrier: None,
        timeout_secs: Some(300), // 5 minute default for validation
        optional: false,
        config: StageSpecificConfig::Validation,
    });
    order += 1;

    // Stage 2: Prepare (if prepare section exists)
    if let Some(ref prepare) = config.prepare {
        // Calculate expected objects from ensure_objects
        let expected_objects = if !prepare.ensure_objects.is_empty() {
            Some(prepare.ensure_objects.iter().map(|e| e.count as usize).sum())
        } else {
            None
        };

        stages.push(StageConfig {
            name: "prepare".to_string(),
            order,
            completion: CompletionCriteria::TasksDone,
            barrier: None,
            timeout_secs: None, // No timeout for prepare (can take hours)
            optional: false,
            config: StageSpecificConfig::Prepare { expected_objects },
        });
        order += 1;
    }

    // Stage 3: Execute (workload)
    let duration = config.duration;

    stages.push(StageConfig {
        name: "execute".to_string(),
        order,
        completion: CompletionCriteria::Duration,
        barrier: None,
        timeout_secs: None,
        optional: false,
        config: StageSpecificConfig::Execute { duration },
    });
    order += 1;

    // Stage 4: Cleanup (if prepare.cleanup is true)
    if let Some(ref prepare) = config.prepare {
        // Check cleanup field
        let cleanup_enabled = prepare.cleanup;

        if cleanup_enabled {
            // Calculate expected objects from ensure_objects
            let expected_objects = if !prepare.ensure_objects.is_empty() {
                Some(prepare.ensure_objects.iter().map(|e| e.count as usize).sum())
            } else {
                None
            };

            stages.push(StageConfig {
                name: "cleanup".to_string(),
                order,
                completion: CompletionCriteria::TasksDone,
                barrier: None,
                timeout_secs: None,
                optional: false,
                config: StageSpecificConfig::Cleanup { expected_objects },
            });
        }
    }

    Ok(stages)
}

/// Convert legacy config to explicit stages format
///
/// Modifies the config in-place:
/// - Adds distributed.stages section
/// - Removes top-level duration (moved to execute stage)
/// - Preserves all other fields
pub fn convert_config(config: &mut Config) -> Result<()> {
    // Generate stages
    let stages = generate_stages(config)
        .context("Failed to generate stages from legacy config")?;

    // Add stages to distributed section (create if needed)
    let distributed = config
        .distributed
        .get_or_insert_with(|| DistributedConfig {
            agents: Vec::new(),
            ssh: None,
            deployment: None,
            start_delay: 2,  // default_start_delay
            path_template: "agent-{id}/".to_string(),  // default_path_template
            shared_filesystem: false,
            tree_creation_mode: crate::config::TreeCreationMode::Isolated,
            path_selection: crate::config::PathSelectionStrategy::Random,
            partition_overlap: 0.3,  // default_partition_overlap
            grpc_keepalive_interval: 30,  // default
            grpc_keepalive_timeout: 10,  // default
            agent_ready_timeout: 120,  // default
            barrier_sync: Default::default(),
            stages: Vec::new(),
            kv_cache_dir: None,
        });

    distributed.stages = stages;

    // Note: We do NOT remove the top-level duration field because:
    // 1. It's still used as a default in Config::default()
    // 2. Removing it would break backward compatibility
    // 3. The execute stage duration takes precedence when stages are present

    Ok(())
}

/// Convert a YAML file in-place, creating a backup
///
/// Steps:
/// 1. Read original YAML as raw text
/// 2. Parse as generic YAML Value (preserves structure)
/// 3. Check if conversion needed
/// 4. Generate stages YAML snippet
/// 5. Insert stages into distributed section (preserving comments/formatting)
/// 6. Write modified YAML
/// 7. Validate (if requested)
/// 8. Create backup and replace original
pub fn convert_yaml_file(
    yaml_path: &Path,
    validate: bool,
    dry_run: bool,
) -> Result<ConversionResult> {
    // Read original YAML as text (preserves comments)
    let yaml_content = std::fs::read_to_string(yaml_path)
        .with_context(|| format!("Failed to read YAML file: {}", yaml_path.display()))?;

    // Parse as Config to check conversion needs
    let config: Config = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse YAML file: {}", yaml_path.display()))?;

    // Check if conversion needed
    if has_stages(&config) {
        return Ok(ConversionResult::AlreadyHasStages);
    }

    if !needs_conversion(&config) {
        return Ok(ConversionResult::NoConversionNeeded);
    }

    // Generate stages configuration
    let stages = generate_stages(&config)
        .context("Failed to generate stages from legacy config")?;
    
    let stage_count = stages.len();

    if dry_run {
        return Ok(ConversionResult::DryRun { stage_count });
    }

    // Convert to minimal YAML representation (only non-default values)
    let stages_yaml = generate_minimal_stages_yaml(&stages)?;

    // Insert stages into YAML text (preserving comments and formatting)
    let new_yaml = insert_stages_into_yaml(&yaml_content, &stages_yaml, &config)?;

    // Write to temp file
    let tmp_path = yaml_path.with_extension("yaml.tmp");
    std::fs::write(&tmp_path, &new_yaml)
        .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;

    // Validate if requested
    if validate {
        // Re-parse to validate the modified YAML
        let validated_config: Config = serde_yaml::from_str(&new_yaml)
            .context("Failed to parse converted YAML")?;
        
        if let Err(e) = crate::validation::display_config_summary(&validated_config, tmp_path.to_str().unwrap()) {
            std::fs::remove_file(&tmp_path).ok();
            anyhow::bail!("Validation failed: {}", e);
        }
    }

    // Create backup
    let backup_path = yaml_path.with_extension("yaml.bak");
    std::fs::copy(yaml_path, &backup_path)
        .with_context(|| format!("Failed to create backup: {}", backup_path.display()))?;

    // Replace original with converted version
    std::fs::rename(&tmp_path, yaml_path)
        .with_context(|| format!("Failed to replace original file: {}", yaml_path.display()))?;

    Ok(ConversionResult::Converted {
        stage_count,
        backup_path: backup_path.to_string_lossy().to_string(),
    })
}

/// Generate minimal YAML for stages (only non-default values)
fn generate_minimal_stages_yaml(stages: &[StageConfig]) -> Result<String> {
    use std::fmt::Write;
    
    let mut yaml = String::new();
    
    for stage in stages {
        writeln!(yaml, "  - name: {}", stage.name)?;
        writeln!(yaml, "    order: {}", stage.order)?;
        
        // Write completion criteria
        let completion_str = match &stage.completion {
            CompletionCriteria::ValidationPassed => "validation_passed",
            CompletionCriteria::TasksDone => "tasks_done",
            CompletionCriteria::Duration => "duration",
            CompletionCriteria::ScriptExit => "script_exit",
            CompletionCriteria::DurationOrTasks => "duration_or_tasks",
        };
        writeln!(yaml, "    completion: {}", completion_str)?;
        
        // Only write timeout if specified (300 is default for some stages)
        if let Some(timeout) = stage.timeout_secs {
            writeln!(yaml, "    timeout_secs: {}", timeout)?;
        }
        
        // Write stage type and type-specific fields
        match &stage.config {
            StageSpecificConfig::Validation => {
                writeln!(yaml, "    type: validation")?;
            }
            StageSpecificConfig::Prepare { expected_objects } => {
                writeln!(yaml, "    type: prepare")?;
                if let Some(count) = expected_objects {
                    writeln!(yaml, "    expected_objects: {}", count)?;
                }
            }
            StageSpecificConfig::Execute { duration } => {
                writeln!(yaml, "    type: execute")?;
                // Format duration nicely (e.g., "5m" instead of "300s")
                let dur_str = format_duration(duration);
                writeln!(yaml, "    duration: {}", dur_str)?;
            }
            StageSpecificConfig::Cleanup { expected_objects } => {
                writeln!(yaml, "    type: cleanup")?;
                if let Some(count) = expected_objects {
                    writeln!(yaml, "    expected_objects: {}", count)?;
                }
            }
            StageSpecificConfig::Custom { command, args } => {
                writeln!(yaml, "    type: custom")?;
                writeln!(yaml, "    command: {}", command)?;
                if !args.is_empty() {
                    writeln!(yaml, "    args:")?;
                    for arg in args {
                        writeln!(yaml, "      - {}", arg)?;
                    }
                }
            }
            StageSpecificConfig::Hybrid { max_duration, expected_tasks } => {
                writeln!(yaml, "    type: hybrid")?;
                if let Some(dur) = max_duration {
                    let dur_str = format_duration(dur);
                    writeln!(yaml, "    max_duration: {}", dur_str)?;
                }
                if let Some(tasks) = expected_tasks {
                    writeln!(yaml, "    expected_tasks: {}", tasks)?;
                }
            }
        }
    }
    
    Ok(yaml)
}

/// Format duration in human-readable format
fn format_duration(duration: &std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs % 60 == 0 && secs >= 60 {
        format!("{}m", secs / 60)
    } else if secs % 3600 == 0 && secs >= 3600 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}s", secs)
    }
}

/// Insert stages into YAML text, preserving comments and formatting
fn insert_stages_into_yaml(original: &str, stages_yaml: &str, config: &Config) -> Result<String> {
    let mut result = String::new();
    let mut in_distributed = false;
    let mut distributed_indent = 0;
    let mut found_distributed = false;
    
    // Check if this is a multi-agent distributed config (needs barriers)
    let is_multi_agent = config.distributed.as_ref()
        .map(|d| d.agents.len() > 1)
        .unwrap_or(false);
    
    for line in original.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        
        // Check if we're entering distributed section
        if trimmed.starts_with("distributed:") {
            in_distributed = true;
            found_distributed = true;
            distributed_indent = indent;
            result.push_str(line);
            result.push('\n');
            continue;
        }
        
        // Check if we're leaving distributed section
        if in_distributed && !trimmed.is_empty() && !trimmed.starts_with('#') && indent <= distributed_indent {
            // We've left the distributed section - insert stages before this line
            insert_stages_section(&mut result, stages_yaml, is_multi_agent);
            in_distributed = false;
        }
        
        result.push_str(line);
        result.push('\n');
    }
    
    // If we're still in distributed section at end of file, append stages
    if in_distributed {
        insert_stages_section(&mut result, stages_yaml, is_multi_agent);
    }
    
    // If no distributed section exists, create one with empty agents
    if !found_distributed {
        result.push_str("\n# Distributed configuration (required for explicit stages)\n");
        result.push_str("distributed:\n");
        result.push_str("  agents: []  # Empty for standalone configs\n");
        insert_stages_section(&mut result, stages_yaml, is_multi_agent);
    }
    
    Ok(result)
}

/// Insert stages section with optional barrier recommendations
fn insert_stages_section(result: &mut String, stages_yaml: &str, is_multi_agent: bool) {
    result.push('\n');
    
    // Add barrier recommendations for multi-agent configs
    if is_multi_agent {
        result.push_str("  # ⚠️  RECOMMENDED: Add barrier synchronization for distributed workloads\n");
        result.push_str("  # Without barriers, agents may race between stages causing incorrect results.\n");
        result.push_str("  # Uncomment the following to enable barriers:\n");
        result.push_str("  #\n");
        result.push_str("  # barrier_sync:\n");
        result.push_str("  #   enabled: true\n");
        result.push_str("  #\n");
        result.push_str("  # Or add per-stage barriers (see example below with \"# ADD THIS\"):\n");
        result.push('\n');
    }
    
    result.push_str("  stages:\n");
    
    // Insert stages with barrier recommendations if multi-agent
    if is_multi_agent {
        insert_stages_with_barrier_hints(result, stages_yaml);
    } else {
        result.push_str(stages_yaml);
    }
}

/// Insert stages with commented barrier recommendations
fn insert_stages_with_barrier_hints(result: &mut String, stages_yaml: &str) {
    let mut first_stage = true;
    
    for line in stages_yaml.lines() {
        result.push_str(line);
        result.push('\n');
        
        // After "type: validation/prepare/execute/cleanup", suggest barrier
        if line.trim().starts_with("type: ") && first_stage {
            result.push_str("    # ADD THIS (recommended for multi-agent workloads):\n");
            result.push_str("    # barrier:\n");
            result.push_str("    #   barrier_type: all_or_nothing\n");
            first_stage = false;
        } else if line.trim().starts_with("type: ") {
            result.push_str("    # ADD THIS:\n");
            result.push_str("    # barrier:\n");
            result.push_str("    #   barrier_type: all_or_nothing\n");
        }
    }
}

#[derive(Debug)]
pub enum ConversionResult {
    /// File already has stages section
    AlreadyHasStages,
    /// File doesn't need conversion (no distributed/workload)
    NoConversionNeeded,
    /// Dry-run mode - would convert
    DryRun { stage_count: usize },
    /// Successfully converted
    Converted {
        stage_count: usize,
        backup_path: String,
    },
}

impl ConversionResult {
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            ConversionResult::Converted { .. } | ConversionResult::DryRun { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PrepareConfig, WeightedOp, OpSpec};
    use std::time::Duration;

    fn create_legacy_config() -> Config {
        Config {
            duration: Duration::from_secs(60),
            concurrency: 16,
            target: Some("file:///tmp/test/".to_string()),
            workload: vec![WeightedOp {
                weight: 100,
                spec: OpSpec::Get {
                    path: "data/*".to_string(),
                    use_multi_endpoint: false,
                },
                concurrency: None,
            }],
            prepare: Some(PrepareConfig {
                cleanup: true,  // Enable cleanup to generate 4 stages in tests
                ..Default::default()
            }),
            distributed: Some(DistributedConfig {
                agents: Vec::new(),
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: false,
                tree_creation_mode: crate::config::TreeCreationMode::Isolated,
                path_selection: crate::config::PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                grpc_keepalive_interval: 30,
                grpc_keepalive_timeout: 10,
                agent_ready_timeout: 120,
                barrier_sync: Default::default(),
                stages: Vec::new(), // Legacy: no stages
                kv_cache_dir: None,
            }),
            range_engine: None,
            page_cache_mode: None,
            io_rate: None,
            processes: None,
            processing_mode: Default::default(),
            live_stats_tracker: None,
            error_handling: Default::default(),
            op_log_path: None,
            warmup_period: None,
            perf_log: None,
            multi_endpoint: None,
            cache_checkpoint_interval_secs: 300,
        }
    }

    #[test]
    fn test_needs_conversion_legacy() {
        let config = create_legacy_config();
        assert!(needs_conversion(&config), "Legacy config should need conversion");
    }

    #[test]
    fn test_needs_conversion_standalone() {
        // Standalone configs (no distributed section) should also be converted in v0.8.61+
        let mut config = create_legacy_config();
        config.distributed = None;
        assert!(needs_conversion(&config), "Standalone config with workload should need conversion");
    }

    #[test]
    fn test_needs_conversion_no_workload() {
        let mut config = create_legacy_config();
        config.workload = Vec::new();
        assert!(!needs_conversion(&config), "Config without workload shouldn't need conversion");
    }

    #[test]
    fn test_has_stages_empty() {
        let config = create_legacy_config();
        assert!(!has_stages(&config), "Legacy config should not have stages");
    }

    #[test]
    fn test_generate_stages_with_prepare_and_cleanup() {
        let config = create_legacy_config();
        let stages = generate_stages(&config).expect("Should generate stages");

        assert_eq!(stages.len(), 4, "Should generate 4 stages");

        // Check stage names and order
        assert_eq!(stages[0].name, "preflight");
        assert_eq!(stages[0].order, 1);
        assert!(matches!(stages[0].config, StageSpecificConfig::Validation));

        assert_eq!(stages[1].name, "prepare");
        assert_eq!(stages[1].order, 2);
        assert!(matches!(stages[1].config, StageSpecificConfig::Prepare { .. }));

        assert_eq!(stages[2].name, "execute");
        assert_eq!(stages[2].order, 3);
        if let StageSpecificConfig::Execute { duration } = stages[2].config {
            assert_eq!(duration.as_secs(), 60);
        } else {
            panic!("Execute stage should have Execute config");
        }

        assert_eq!(stages[3].name, "cleanup");
        assert_eq!(stages[3].order, 4);
        assert!(matches!(stages[3].config, StageSpecificConfig::Cleanup { .. }));
    }

    #[test]
    fn test_generate_stages_no_prepare() {
        let mut config = create_legacy_config();
        config.prepare = None;

        let stages = generate_stages(&config).expect("Should generate stages");

        assert_eq!(stages.len(), 2, "Should generate 2 stages (preflight + execute only)");
        assert_eq!(stages[0].name, "preflight");
        assert_eq!(stages[1].name, "execute");
    }

    #[test]
    fn test_generate_stages_no_cleanup() {
        let mut config = create_legacy_config();
        if let Some(ref mut prepare) = config.prepare {
            prepare.cleanup = false;
        }

        let stages = generate_stages(&config).expect("Should generate stages");

        assert_eq!(stages.len(), 3, "Should generate 3 stages (no cleanup)");
        assert_eq!(stages[0].name, "preflight");
        assert_eq!(stages[1].name, "prepare");
        assert_eq!(stages[2].name, "execute");
    }

    #[test]
    fn test_convert_config() {
        let mut config = create_legacy_config();
        let original_duration = config.duration;

        convert_config(&mut config).expect("Should convert config");

        // Check that stages were added
        assert!(has_stages(&config), "Config should now have stages");

        let distributed = config.distributed.as_ref().unwrap();
        assert_eq!(distributed.stages.len(), 4);

        // Verify execute stage has the original duration
        let execute_stage = distributed.stages.iter().find(|s| s.name == "execute").unwrap();
        if let StageSpecificConfig::Execute { duration } = execute_stage.config {
            assert_eq!(duration, original_duration);
        }
    }

    #[test]
    fn test_convert_standalone_config() {
        // Create a standalone config (no distributed section)
        let mut config = Config {
            duration: Duration::from_secs(30),
            concurrency: 8,
            target: Some("file:///tmp/test/".to_string()),
            workload: vec![WeightedOp {
                weight: 100,
                spec: OpSpec::Get {
                    path: "data/*".to_string(),
                    use_multi_endpoint: false,
                },
                concurrency: None,
            }],
            prepare: None,  // No prepare = no cleanup stage
            distributed: None, // Key: No distributed section!
            range_engine: None,
            page_cache_mode: None,
            io_rate: None,
            processes: None,
            processing_mode: Default::default(),
            live_stats_tracker: None,
            error_handling: Default::default(),
            op_log_path: None,
            warmup_period: None,
            perf_log: None,
            multi_endpoint: None,
            cache_checkpoint_interval_secs: 300,
        };

        let original_duration = config.duration;

        convert_config(&mut config).expect("Should convert standalone config");

        // Check that distributed section was created with stages
        assert!(config.distributed.is_some(), "Distributed section should be created");
        assert!(has_stages(&config), "Config should now have stages");

        let distributed = config.distributed.as_ref().unwrap();
        
        // Should have 2 stages: preflight + execute (no prepare, no cleanup)
        assert_eq!(distributed.stages.len(), 2, "Should generate 2 stages for minimal config");
        assert_eq!(distributed.stages[0].name, "preflight");
        assert_eq!(distributed.stages[1].name, "execute");

        // Verify execute stage has the original duration
        let execute_stage = &distributed.stages[1];
        if let StageSpecificConfig::Execute { duration } = execute_stage.config {
            assert_eq!(duration, original_duration);
        } else {
            panic!("Execute stage should have Execute config");
        }

        // Verify agents list is empty (standalone mode)
        assert!(distributed.agents.is_empty(), "Standalone config should have no agents");
    }

    #[test]
    fn test_convert_standalone_with_prepare() {
        // Standalone config with prepare section
        let mut config = Config {
            duration: Duration::from_secs(60),
            concurrency: 16,
            target: Some("s3://bucket/test/".to_string()),
            workload: vec![
                WeightedOp {
                    weight: 70,
                    spec: OpSpec::Get {
                        path: "objects/*".to_string(),
                        use_multi_endpoint: false,
                    },
                    concurrency: None,
                },
                WeightedOp {
                    weight: 30,
                    spec: OpSpec::Put {
                        path: "uploads/".to_string(),
                        object_size: Some(1048576),
                        size_spec: None,
                        dedup_factor: 1,
                        compress_factor: 1,
                        use_multi_endpoint: false,
                    },
                    concurrency: None,
                },
            ],
            prepare: Some(PrepareConfig {
                cleanup: false,  // No cleanup stage
                ..Default::default()
            }),
            distributed: None,
            range_engine: None,
            page_cache_mode: None,
            io_rate: None,
            processes: None,
            processing_mode: Default::default(),
            live_stats_tracker: None,
            error_handling: Default::default(),
            op_log_path: None,
            warmup_period: None,
            perf_log: None,
            multi_endpoint: None,
            cache_checkpoint_interval_secs: 300,
        };

        convert_config(&mut config).expect("Should convert config");

        let distributed = config.distributed.as_ref().unwrap();
        
        // Should have 3 stages: preflight + prepare + execute (no cleanup)
        assert_eq!(distributed.stages.len(), 3);
        assert_eq!(distributed.stages[0].name, "preflight");
        assert_eq!(distributed.stages[1].name, "prepare");
        assert_eq!(distributed.stages[2].name, "execute");
    }
}
