//! Distributed configuration validation for sai3-bench
//!
//! Validates distributed workload configurations to catch common issues like:
//! - base_uri mismatches in isolated mode with multi-endpoint
//! - Agents accessing unavailable storage endpoints
//! - Tree creation mode conflicts

use super::{ValidationResult, ResultLevel, ErrorType};
use crate::config::{Config, TreeCreationMode};
use anyhow::Result;

/// Validate distributed configuration for common issues
///
/// Validates two key scenarios based on shared_filesystem setting:
///
/// 1. shared_filesystem: true (agents access same underlying storage)
///    - All agents should have compatible endpoint configurations
///    - Different mount points are OK (e.g., NFS mounted at different paths)
///    - base_uri usage is flexible - can be shared or omitted
///
/// 2. shared_filesystem: false (per-agent storage, independent)
///    - Each agent has its own storage endpoints
///    - In isolated mode, base_uri should be omitted to avoid listing failures
///    - No assumptions about endpoint accessibility
pub fn validate_distributed_config(config: &Config) -> Result<Vec<ValidationResult>> {
    let mut results = Vec::new();

    // Only validate if this is a distributed configuration
    if let Some(ref dist) = config.distributed {
        if dist.agents.is_empty() {
            return Ok(results); // No distributed mode
        }

        // Validate based on shared_filesystem setting
        if let Some(ref prepare) = config.prepare {
            for spec in &prepare.ensure_objects {
                
                if dist.shared_filesystem {
                    // SHARED STORAGE: All agents access same underlying storage
                    // Validate that multi_endpoint configs are compatible
                    if spec.use_multi_endpoint {
                        // Check if all agents have multi_endpoint configured
                        let agents_without_multiep: Vec<String> = dist.agents.iter()
                            .filter(|a| a.multi_endpoint.is_none())
                            .map(|a| a.id.as_deref().unwrap_or(&a.address).to_string())
                            .collect();
                        
                        if !agents_without_multiep.is_empty() {
                            results.push(ValidationResult {
                                level: ResultLevel::Warning,
                                error_type: None,
                                message: format!(
                                    "{} agents don't have multi_endpoint configured (shared_filesystem: true)",
                                    agents_without_multiep.len()
                                ),
                                suggestion: format!(
                                    "With shared_filesystem: true and use_multi_endpoint: true, all agents\n\
                                     should have multi_endpoint configurations pointing to the shared storage.\n\n\
                                     Agents without multi_endpoint: {}\n\n\
                                     These agents will use the global target URI instead, which may work\n\
                                     but prevents per-agent endpoint customization.",
                                    agents_without_multiep.join(", ")
                                ),
                                details: None,
                                test_phase: "config_validation".to_string(),
                            });
                        }
                    }
                } else {
                    // PER-AGENT STORAGE: Each agent has independent storage
                    // THE BUG: base_uri specified in isolated mode with use_multi_endpoint
                    // This causes listing to use base_uri instead of each agent's first endpoint
                    if spec.use_multi_endpoint 
                        && spec.base_uri.is_some() 
                        && dist.tree_creation_mode == TreeCreationMode::Isolated 
                    {
                        results.push(ValidationResult {
                            level: ResultLevel::Error,
                            error_type: Some(ErrorType::Configuration),
                            message: "base_uri should not be specified in isolated mode with per-agent storage".to_string(),
                            suggestion: format!(
                                "RECOMMENDED FIX: Remove 'base_uri' field from ensure_objects.\n\n\
                                 Current configuration:\n\
                                 - tree_creation_mode: isolated (each agent creates separate tree)\n\
                                 - use_multi_endpoint: true (multi-endpoint load balancing)\n\
                                 - shared_filesystem: false (per-agent storage)\n\
                                 - base_uri: {} (PROBLEM - causes listing failures)\n\n\
                                 When base_uri is omitted, each agent uses its FIRST multi_endpoint URI\n\
                                 for prepare/listing operations, which matches the isolated storage model.\n\n\
                                 Valid alternatives:\n\
                                 - Remove base_uri (recommended for isolated mode)\n\
                                 - Set shared_filesystem: true (if agents actually share storage)\n\
                                 - Set tree_creation_mode: concurrent/coordinator (if sharing tree)",
                                spec.base_uri.as_ref().unwrap()
                            ),
                            details: Some(format!(
                                "This configuration causes agents to list objects from '{}' during\n\
                                 the prepare phase, which may not match their configured multi_endpoint.\n\
                                 This leads to h2 protocol errors or empty listings when agents try to\n\
                                 access storage they're not configured to use.",
                                spec.base_uri.as_ref().unwrap()
                            )),
                            test_phase: "config_validation".to_string(),
                        });
                    }
                }
                
                // Check for missing base_uri when NOT using multi_endpoint
                if !spec.use_multi_endpoint && spec.base_uri.is_none() {
                    results.push(ValidationResult {
                        level: ResultLevel::Error,
                        error_type: Some(ErrorType::Configuration),
                        message: "base_uri is required when use_multi_endpoint=false".to_string(),
                        suggestion: "Add 'base_uri' field to ensure_objects, or set use_multi_endpoint: true".to_string(),
                        details: Some(
                            "When use_multi_endpoint is false, base_uri specifies where objects\n\
                             are created and accessed.".to_string()
                        ),
                        test_phase: "config_validation".to_string(),
                    });
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, DistributedConfig, AgentConfig, MultiEndpointConfig, PrepareConfig, EnsureSpec,
        FillPattern, TreeCreationMode, PathSelectionStrategy,
    };

    /// Helper to create a minimal Config for testing
    fn create_test_config(target: Option<String>, distributed: Option<DistributedConfig>, prepare: Option<PrepareConfig>) -> Config {
        Config {
            duration: std::time::Duration::from_secs(60),
            concurrency: 8,
            target,
            workload: Vec::new(),
            prepare,
            range_engine: None,
            page_cache_mode: None,
            distributed,
            io_rate: None,
            processes: None,
            processing_mode: crate::config::ProcessingMode::MultiRuntime,
            live_stats_tracker: None,
            error_handling: crate::config::ErrorHandlingConfig::default(),
            op_log_path: None,
            warmup_period: None,
            perf_log: None,
            multi_endpoint: None,
        }
    }

    /// Helper to create a minimal agent config
    fn create_agent(id: &str, endpoints: Vec<String>) -> AgentConfig {
        AgentConfig {
            address: "127.0.0.1:7761".to_string(),
            id: Some(id.to_string()),
            target_override: None,
            concurrency_override: None,
            env: std::collections::HashMap::new(),
            volumes: Vec::new(),
            path_template: None,
            listen_port: 7761,
            multi_endpoint: Some(MultiEndpointConfig {
                endpoints,
                strategy: "round_robin".to_string(),
            }),
        }
    }

    #[test]
    fn test_detect_base_uri_in_isolated_mode() {
        // THE BUG: base_uri specified in isolated mode with per-agent storage
        let config = create_test_config(
            Some("file:///test/".to_string()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["file:///mnt/vast1/benchmark/".to_string()]),
                    create_agent("agent-2", vec!["file:///mnt/vast5/benchmark/".to_string()]),
                    create_agent("agent-3", vec!["file:///mnt/vast9/benchmark/".to_string()]),
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: false,  // Per-agent storage
                tree_creation_mode: TreeCreationMode::Isolated,  // Each agent creates separate tree
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                barrier_sync: BarrierSyncConfig::default(),  // No barrier sync for tests
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: Some("file:///mnt/vast1/benchmark/".to_string()), // THE BUG
                    use_multi_endpoint: true,
                    count: 100,
                    min_size: None,
                    max_size: None,
                    size_spec: None,
                    fill: FillPattern::Zero,
                    dedup_factor: 1,
                    compress_factor: 1,
                }],
                cleanup: false,
                cleanup_only: None,
                post_prepare_delay: 0,
                directory_structure: None,
                prepare_strategy: crate::config::PrepareStrategy::Sequential,
                skip_verification: false,
                force_overwrite: false,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();
        
        // Should detect the error
        assert!(!results.is_empty(), "Should detect base_uri in isolated mode");
        let error = results.iter().find(|r| r.level == ResultLevel::Error);
        assert!(error.is_some(), "Should have an error result");
        
        let error = error.unwrap();
        assert_eq!(error.error_type, Some(ErrorType::Configuration));
        assert!(error.message.contains("isolated mode"));
        assert!(error.suggestion.contains("Remove 'base_uri'"));
    }

    #[test]
    fn test_valid_shared_filesystem_with_base_uri() {
        // VALID: Shared filesystem - all agents access same storage
        // base_uri is fine here because agents are expected to share storage
        let shared_uri = "file:///shared/data/".to_string();
        
        let config = create_test_config(
            Some(shared_uri.clone()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["file:///mnt/node1/shared/".to_string()]),
                    create_agent("agent-2", vec!["file:///mnt/node2/shared/".to_string()]),
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: true,  // SHARED - base_uri is OK
                tree_creation_mode: TreeCreationMode::Concurrent,
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                barrier_sync: BarrierSyncConfig::default(),  // No barrier sync for tests
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: Some(shared_uri),
                    use_multi_endpoint: true,
                    count: 100,
                    min_size: None,
                    max_size: None,
                    size_spec: None,
                    fill: FillPattern::Zero,
                    dedup_factor: 1,
                    compress_factor: 1,
                }],
                cleanup: false,
                cleanup_only: None,
                post_prepare_delay: 0,
                directory_structure: None,
                prepare_strategy: crate::config::PrepareStrategy::Sequential,
                skip_verification: false,
                force_overwrite: false,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();
        
        // Should have NO errors or warnings - this is a valid configuration
        let has_error = results.iter().any(|r| r.level == ResultLevel::Error);
        assert!(!has_error, "Shared filesystem with base_uri should be valid");
        
        let has_warning = results.iter().any(|r| r.level == ResultLevel::Warning);
        assert!(!has_warning, "Shared filesystem with base_uri should not warn");
    }

    #[test]
    fn test_valid_isolated_mode_no_base_uri() {
        // CORRECT PATTERN: Isolated mode without base_uri
        // Each agent uses its own first multi_endpoint for prepare/listing
        let config = create_test_config(
            Some("file:///test/".to_string()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["file:///mnt/vast1/benchmark/".to_string()]),
                    create_agent("agent-2", vec!["file:///mnt/vast5/benchmark/".to_string()]),
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: false,  // Per-agent storage
                tree_creation_mode: TreeCreationMode::Isolated,  // Isolated trees
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                barrier_sync: BarrierSyncConfig::default(),  // No barrier sync for tests
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: None, // THE FIX - each agent uses its own first endpoint
                    use_multi_endpoint: true,
                    count: 100,
                    min_size: None,
                    max_size: None,
                    size_spec: None,
                    fill: FillPattern::Zero,
                    dedup_factor: 1,
                    compress_factor: 1,
                }],
                cleanup: false,
                cleanup_only: None,
                post_prepare_delay: 0,
                directory_structure: None,
                prepare_strategy: crate::config::PrepareStrategy::Sequential,
                skip_verification: false,
                force_overwrite: false,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();
        
        // Should have no errors or warnings - this is the recommended pattern
        let has_issues = results.iter().any(|r| {
            r.level == ResultLevel::Error || r.level == ResultLevel::Warning
        });
        assert!(!has_issues, "Isolated mode without base_uri should be valid");
    }

    #[test]
    fn test_shared_filesystem_missing_multiep_warning() {
        // VALID but SUBOPTIMAL: Shared filesystem where not all agents have multi_endpoint
        let config = create_test_config(
            Some("s3://shared-bucket/".to_string()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["s3://shared-bucket/".to_string()]),
                    // Agent 2 has no multi_endpoint - will use global target
                    AgentConfig {
                        address: "127.0.0.1:7762".to_string(),
                        id: Some("agent-2".to_string()),
                        target_override: None,
                        concurrency_override: None,
                        env: std::collections::HashMap::new(),
                        volumes: Vec::new(),
                        path_template: None,
                        listen_port: 7762,
                        multi_endpoint: None,  // Missing multi_endpoint
                    },
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: true,  // SHARED storage
                tree_creation_mode: TreeCreationMode::Concurrent,
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                barrier_sync: BarrierSyncConfig::default(),  // No barrier sync for tests
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: Some("s3://shared-bucket/test/".to_string()),
                    use_multi_endpoint: true,
                    count: 100,
                    min_size: None,
                    max_size: None,
                    size_spec: None,
                    fill: FillPattern::Zero,
                    dedup_factor: 1,
                    compress_factor: 1,
                }],
                cleanup: false,
                cleanup_only: None,
                post_prepare_delay: 0,
                directory_structure: None,
                prepare_strategy: crate::config::PrepareStrategy::Sequential,
                skip_verification: false,
                force_overwrite: false,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();
        
        // Should have a warning about missing multi_endpoint
        let has_warning = results.iter().any(|r| {
            r.level == ResultLevel::Warning 
                && r.message.contains("don't have multi_endpoint")
        });
        assert!(has_warning, "Should warn about agents without multi_endpoint in shared mode");
        
        // Should NOT have errors - this is valid, just not optimal
        let has_error = results.iter().any(|r| r.level == ResultLevel::Error);
        assert!(!has_error, "Should not error on missing multi_endpoint in shared mode");
    }
}
