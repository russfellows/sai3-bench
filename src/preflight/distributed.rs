//! Distributed configuration validation for sai3-bench
//!
//! Validates distributed workload configurations to catch common issues like:
//! - base_uri mismatches in isolated mode with multi-endpoint
//! - Agents accessing unavailable storage endpoints
//! - Tree creation mode conflicts

use super::{ErrorType, ResultLevel, ValidationResult};
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
                    let any_agent_has_multi_ep =
                        dist.agents.iter().any(|a| a.multi_endpoint.is_some());
                    if any_agent_has_multi_ep || config.multi_endpoint.is_some() {
                        // Check if all agents have multi_endpoint configured
                        let agents_without_multiep: Vec<String> = dist
                            .agents
                            .iter()
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
                                    "With shared_filesystem: true and multi_endpoint configured, all agents\n\
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
                    // Detect: base_uri specified in isolated mode when agents have their own endpoints
                    // This causes listing to use base_uri instead of each agent's first endpoint
                    let any_agent_has_multi_ep =
                        dist.agents.iter().any(|a| a.multi_endpoint.is_some());
                    if any_agent_has_multi_ep
                        && spec.base_uri.is_some()
                        && dist.tree_creation_mode == TreeCreationMode::Isolated
                    {
                        if let Some(base_uri) = spec.base_uri.as_ref() {
                            results.push(ValidationResult {
                            level: ResultLevel::Error,
                            error_type: Some(ErrorType::Configuration),
                            message: "base_uri should not be specified in isolated mode with per-agent storage".to_string(),
                            suggestion: format!(
                                "RECOMMENDED FIX: Remove 'base_uri' field from ensure_objects.\n\n\
                                 Current configuration:\n\
                                 - tree_creation_mode: isolated (each agent creates separate tree)\n\
                                 - multi_endpoint: configured (multi-endpoint load balancing)\n\
                                 - shared_filesystem: false (per-agent storage)\n\
                                 - base_uri: {} (PROBLEM - causes listing failures)\n\n\
                                 When base_uri is omitted, each agent uses its FIRST multi_endpoint URI\n\
                                 for prepare/listing operations, which matches the isolated storage model.\n\n\
                                 Valid alternatives:\n\
                                 - Remove base_uri (recommended for isolated mode)\n\
                                 - Set shared_filesystem: true (if agents actually share storage)\n\
                                 - Set tree_creation_mode: concurrent/coordinator (if sharing tree)",
                                base_uri
                            ),
                            details: Some(format!(
                                "This configuration causes agents to list objects from '{}' during\n\
                                 the prepare phase, which may not match their configured multi_endpoint.\n\
                                 This leads to h2 protocol errors or empty listings when agents try to\n\
                                 access storage they're not configured to use.",
                                base_uri
                            )),
                            test_phase: "config_validation".to_string(),
                        });
                        }
                    }
                }

                // Check for missing base_uri when no multi_endpoint configuration is present
                let any_multi_ep = config.multi_endpoint.is_some()
                    || dist.agents.iter().any(|a| a.multi_endpoint.is_some());
                if !any_multi_ep && spec.base_uri.is_none() {
                    results.push(ValidationResult {
                        level: ResultLevel::Error,
                        error_type: Some(ErrorType::Configuration),
                        message: "base_uri is required when no multi_endpoint configuration is present".to_string(),
                        suggestion: "Add 'base_uri' field to ensure_objects, or add a multi_endpoint configuration".to_string(),
                        details: Some(
                            "When no multi_endpoint is configured, base_uri specifies where objects\n\
                             are created and accessed.".to_string()
                        ),
                        test_phase: "config_validation".to_string(),
                    });
                }
            }
        }

        // --- Redundant top-level multi_endpoint check ---
        // When every agent has its own per-agent multi_endpoint, the top-level
        // multi_endpoint serves no purpose during distributed runs — the controller
        // forwards the full config YAML to each agent and, before this fix, the agent
        // used the top-level endpoints for preflight instead of its own.  Even with
        // the fix, a top-level union of all agents' endpoints is confusing and likely
        // wrong for standalone ("sai3-bench run") invocations.
        if let Some(ref multi) = config.multi_endpoint {
            if !multi.endpoints.is_empty() {
                let agents_with_own_endpoints = dist
                    .agents
                    .iter()
                    .filter(|a| {
                        a.multi_endpoint
                            .as_ref()
                            .is_some_and(|m| !m.endpoints.is_empty())
                    })
                    .count();

                if agents_with_own_endpoints == dist.agents.len() && dist.agents.len() > 1 {
                    // All agents have per-agent endpoints — top-level is redundant
                    results.push(ValidationResult {
                        level: ResultLevel::Warning,
                        error_type: Some(ErrorType::Configuration),
                        message: format!(
                            "Top-level multi_endpoint ({} URIs) is redundant: all {} agents \
                             have their own per-agent multi_endpoint",
                            multi.endpoints.len(),
                            dist.agents.len()
                        ),
                        suggestion:
                            "In distributed mode each agent uses its own per-agent multi_endpoint.\n\
                             The top-level multi_endpoint is only used by standalone (single-node) runs.\n\
                             Consider removing the top-level multi_endpoint block to avoid confusion,\n\
                             or document why it is intentionally present.".to_string(),
                        details: Some(
                            "During pre-flight validation each agent now correctly validates only\n\
                             its own per-agent endpoints, so this does not cause failures.\n\
                             However a standalone 'sai3-bench run --config ...' would test ALL\n\
                             top-level endpoints, which may not be what you want.".to_string()
                        ),
                        test_phase: "config_validation".to_string(),
                    });
                }
            }
        }

        // --- Credential consistency hint ---
        // We can't check credentials on remote agents from here, but we can tell
        // the user that each agent must have credentials configured independently.
        let has_object_storage = config.multi_endpoint.as_ref().is_some_and(|m| {
            m.endpoints
                .iter()
                .any(|e| e.starts_with("s3://") || e.starts_with("gs://") || e.starts_with("az://"))
        }) || dist.agents.iter().any(|a| {
            a.multi_endpoint.as_ref().is_some_and(|m| {
                m.endpoints.iter().any(|e| {
                    e.starts_with("s3://") || e.starts_with("gs://") || e.starts_with("az://")
                })
            }) || a.target_override.as_ref().is_some_and(|t| {
                t.starts_with("s3://") || t.starts_with("gs://") || t.starts_with("az://")
            })
        });

        if has_object_storage && dist.agents.len() > 1 {
            // Check whether the local environment has credentials set at all.
            // If not, the user may have forgotten to set them on the agents too.
            let has_local_creds = std::env::var("AWS_ACCESS_KEY_ID").is_ok()
                || std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
                || std::env::var("AZURE_STORAGE_ACCOUNT").is_ok();

            if !has_local_creds {
                results.push(ValidationResult {
                    level: ResultLevel::Warning,
                    error_type: Some(ErrorType::Authentication),
                    message: "No cloud credentials found in controller environment".to_string(),
                    suggestion: "Each agent host must have credentials set independently:\n\
                         export AWS_ACCESS_KEY_ID=<key>\n\
                         export AWS_SECRET_ACCESS_KEY=<secret>\n\n\
                         The controller does NOT forward credentials to agents.\n\
                         Set credentials on every agent node before starting sai3bench-agent."
                        .to_string(),
                    details: None,
                    test_phase: "config_validation".to_string(),
                });
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, BarrierSyncConfig, Config, DistributedConfig, EnsureSpec, FillPattern,
        MultiEndpointConfig, PathSelectionStrategy, PrepareConfig, TreeCreationMode,
    };

    /// Helper to create a minimal Config for testing
    fn create_test_config(
        target: Option<String>,
        distributed: Option<DistributedConfig>,
        prepare: Option<PrepareConfig>,
    ) -> Config {
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
            cache_checkpoint_interval_secs: crate::config::default_cache_checkpoint_interval(),
            enable_metadata_cache: true,
            s3dlio_optimization: None,
            distributed_env: std::collections::HashMap::new(),
            dynamic_put_pool: false,
        }
    }

    /// Helper to create a minimal agent config
    fn create_agent(id: &str, endpoints: Vec<String>) -> AgentConfig {
        AgentConfig {
            address: "127.0.0.1:7167".to_string(),
            id: Some(id.to_string()),
            target_override: None,
            concurrency_override: None,
            env: std::collections::HashMap::new(),
            volumes: Vec::new(),
            path_template: None,
            listen_port: 7167,
            multi_endpoint: Some(MultiEndpointConfig {
                endpoints,
                strategy: "round_robin".to_string(),
            }),
            kv_cache_dir: None,
        }
    }

    #[test]
    fn test_detect_base_uri_in_isolated_mode() {
        // THE BUG: base_uri specified in isolated mode with per-agent storage
        let config = create_test_config(
            Some("file:///test/".to_string()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent(
                        "agent-1",
                        vec!["file:///mnt/filesys1/benchmark/".to_string()],
                    ),
                    create_agent(
                        "agent-2",
                        vec!["file:///mnt/filesys5/benchmark/".to_string()],
                    ),
                    create_agent(
                        "agent-3",
                        vec!["file:///mnt/filesys9/benchmark/".to_string()],
                    ),
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: false, // Per-agent storage
                tree_creation_mode: TreeCreationMode::Isolated, // Each agent creates separate tree
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                grpc_keepalive_interval: 30,
                grpc_keepalive_timeout: 10,
                agent_ready_timeout: 120, // v0.8.51: Default timeout
                barrier_sync: BarrierSyncConfig::default(), // No barrier sync for tests
                stages: Vec::new(),       // No YAML stages for tests
                kv_cache_dir: None,
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: Some("file:///mnt/filesys1/benchmark/".to_string()), // THE BUG
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
                key_prefix_shards: 0,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();

        // Should detect the error
        assert!(
            !results.is_empty(),
            "Should detect base_uri in isolated mode"
        );
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
                shared_filesystem: true, // SHARED - base_uri is OK
                tree_creation_mode: TreeCreationMode::Concurrent,
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                grpc_keepalive_interval: 30,
                grpc_keepalive_timeout: 10,
                agent_ready_timeout: 120, // v0.8.51: Default timeout
                barrier_sync: BarrierSyncConfig::default(), // No barrier sync for tests
                stages: Vec::new(),       // No YAML stages for tests
                kv_cache_dir: None,
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: Some(shared_uri),
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
                key_prefix_shards: 0,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();

        // Should have NO errors or warnings - this is a valid configuration
        let has_error = results.iter().any(|r| r.level == ResultLevel::Error);
        assert!(
            !has_error,
            "Shared filesystem with base_uri should be valid"
        );

        let has_warning = results.iter().any(|r| r.level == ResultLevel::Warning);
        assert!(
            !has_warning,
            "Shared filesystem with base_uri should not warn"
        );
    }

    #[test]
    fn test_valid_isolated_mode_no_base_uri() {
        // CORRECT PATTERN: Isolated mode without base_uri
        // Each agent uses its own first multi_endpoint for prepare/listing
        let config = create_test_config(
            Some("file:///test/".to_string()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent(
                        "agent-1",
                        vec!["file:///mnt/filesys1/benchmark/".to_string()],
                    ),
                    create_agent(
                        "agent-2",
                        vec!["file:///mnt/filesys5/benchmark/".to_string()],
                    ),
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: false, // Per-agent storage
                tree_creation_mode: TreeCreationMode::Isolated, // Isolated trees
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                grpc_keepalive_interval: 30,
                grpc_keepalive_timeout: 10,
                agent_ready_timeout: 120, // v0.8.51: Default timeout
                barrier_sync: BarrierSyncConfig::default(), // No barrier sync for tests
                stages: Vec::new(),       // No YAML stages for tests
                kv_cache_dir: None,
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: None, // THE FIX - each agent uses its own first endpoint
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
                key_prefix_shards: 0,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();

        // Should have no errors or warnings - this is the recommended pattern
        let has_issues = results
            .iter()
            .any(|r| r.level == ResultLevel::Error || r.level == ResultLevel::Warning);
        assert!(
            !has_issues,
            "Isolated mode without base_uri should be valid"
        );
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
                        address: "127.0.0.1:7168".to_string(),
                        id: Some("agent-2".to_string()),
                        target_override: None,
                        concurrency_override: None,
                        env: std::collections::HashMap::new(),
                        volumes: Vec::new(),
                        path_template: None,
                        listen_port: 7168,
                        multi_endpoint: None, // Missing multi_endpoint
                        kv_cache_dir: None,
                    },
                ],
                ssh: None,
                deployment: None,
                start_delay: 2,
                path_template: "agent-{id}/".to_string(),
                shared_filesystem: true, // SHARED storage
                tree_creation_mode: TreeCreationMode::Concurrent,
                path_selection: PathSelectionStrategy::Random,
                partition_overlap: 0.3,
                grpc_keepalive_interval: 30,
                grpc_keepalive_timeout: 10,
                agent_ready_timeout: 120, // v0.8.51: Default timeout
                barrier_sync: BarrierSyncConfig::default(), // No barrier sync for tests
                stages: Vec::new(),       // No YAML stages for tests
                kv_cache_dir: None,
            }),
            Some(PrepareConfig {
                ensure_objects: vec![EnsureSpec {
                    base_uri: Some("s3://shared-bucket/test/".to_string()),
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
                key_prefix_shards: 0,
                cleanup_mode: crate::config::CleanupMode::Tolerant,
            }),
        );

        let results = validate_distributed_config(&config).unwrap();

        // Should have a warning about missing multi_endpoint
        let has_warning = results.iter().any(|r| {
            r.level == ResultLevel::Warning && r.message.contains("don't have multi_endpoint")
        });
        assert!(
            has_warning,
            "Should warn about agents without multi_endpoint in shared mode"
        );

        // Should NOT have errors - this is valid, just not optimal
        let has_error = results.iter().any(|r| r.level == ResultLevel::Error);
        assert!(
            !has_error,
            "Should not error on missing multi_endpoint in shared mode"
        );
    }

    // ── Redundant top-level multi_endpoint (v0.8.92) ──────────────────────────

    #[test]
    fn test_warn_redundant_top_level_multi_endpoint() {
        // When every agent has its own per-agent multi_endpoint AND a top-level
        // multi_endpoint also exists → should emit a Warning.
        let mut config = create_test_config(
            None,
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["s3://10.0.0.1/bucket-a/".to_string()]),
                    create_agent("agent-2", vec!["s3://10.0.0.2/bucket-b/".to_string()]),
                ],
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
                barrier_sync: BarrierSyncConfig::default(),
                stages: Vec::new(),
                kv_cache_dir: None,
            }),
            None,
        );
        // Add a top-level multi_endpoint that unions all agent endpoints (redundant pattern)
        config.multi_endpoint = Some(MultiEndpointConfig {
            endpoints: vec![
                "s3://10.0.0.1/bucket-a/".to_string(),
                "s3://10.0.0.2/bucket-b/".to_string(),
            ],
            strategy: "round_robin".to_string(),
        });

        let results = validate_distributed_config(&config).unwrap();

        let redundant_warning = results.iter().find(|r| {
            r.level == ResultLevel::Warning
                && r.message.contains("redundant")
                && r.message.contains("multi_endpoint")
        });
        assert!(
            redundant_warning.is_some(),
            "Should warn that top-level multi_endpoint is redundant when all agents have their own"
        );
    }

    #[test]
    fn test_no_redundant_warning_when_only_some_agents_have_per_agent_endpoints() {
        // If only a subset of agents have per-agent endpoints, the top-level
        // multi_endpoint is still meaningful → no redundancy warning.
        let mut config = create_test_config(
            None,
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["s3://10.0.0.1/bucket-a/".to_string()]),
                    // agent-2 has no per-agent override
                    AgentConfig {
                        address: "host2:7167".to_string(),
                        id: Some("agent-2".to_string()),
                        target_override: None,
                        concurrency_override: None,
                        env: std::collections::HashMap::new(),
                        volumes: Vec::new(),
                        path_template: None,
                        listen_port: 7167,
                        multi_endpoint: None, // ← no per-agent endpoints
                        kv_cache_dir: None,
                    },
                ],
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
                barrier_sync: BarrierSyncConfig::default(),
                stages: Vec::new(),
                kv_cache_dir: None,
            }),
            None,
        );
        config.multi_endpoint = Some(MultiEndpointConfig {
            endpoints: vec!["s3://10.0.0.1/bucket-a/".to_string()],
            strategy: "round_robin".to_string(),
        });

        let results = validate_distributed_config(&config).unwrap();

        let redundant_warning = results.iter().find(|r| {
            r.level == ResultLevel::Warning
                && r.message.contains("redundant")
                && r.message.contains("multi_endpoint")
        });
        assert!(
            redundant_warning.is_none(),
            "Should NOT warn when not all agents have per-agent endpoints"
        );
    }

    // ── Credential hint (v0.8.92) ─────────────────────────────────────────────

    #[test]
    fn test_no_credential_warning_for_filesystem_only_config() {
        // file:// targets don't need cloud credentials → no hint
        let config = create_test_config(
            Some("file:///mnt/data/".to_string()),
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["file:///mnt/node1/".to_string()]),
                    create_agent("agent-2", vec!["file:///mnt/node2/".to_string()]),
                ],
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
                barrier_sync: BarrierSyncConfig::default(),
                stages: Vec::new(),
                kv_cache_dir: None,
            }),
            None,
        );

        let results = validate_distributed_config(&config).unwrap();

        let cred_warning = results.iter().find(|r| {
            r.level == ResultLevel::Warning && r.error_type == Some(ErrorType::Authentication)
        });
        assert!(
            cred_warning.is_none(),
            "file:// configs must not trigger a credential warning"
        );
    }

    #[test]
    fn test_credential_warning_for_s3_config_without_env_creds() {
        // S3 endpoints + multiple agents + no local creds → should warn.
        // We temporarily unset known credential vars to ensure the check fires.
        let _guard = CredentialEnvGuard::save_and_clear();

        let config = create_test_config(
            None,
            Some(DistributedConfig {
                agents: vec![
                    create_agent("agent-1", vec!["s3://10.0.0.1/bucket-a/".to_string()]),
                    create_agent("agent-2", vec!["s3://10.0.0.2/bucket-b/".to_string()]),
                ],
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
                barrier_sync: BarrierSyncConfig::default(),
                stages: Vec::new(),
                kv_cache_dir: None,
            }),
            None,
        );

        let results = validate_distributed_config(&config).unwrap();

        let cred_warning = results.iter().find(|r| {
            r.level == ResultLevel::Warning
                && r.error_type == Some(ErrorType::Authentication)
                && r.message.contains("credentials")
        });
        assert!(
            cred_warning.is_some(),
            "Should warn about missing credentials when using S3 with multiple agents"
        );
    }

    /// RAII guard that saves cloud credential env vars, clears them for a test,
    /// and restores them on drop.  Prevents this test from accidentally masking a
    /// real credential present in the test runner's environment.
    struct CredentialEnvGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl CredentialEnvGuard {
        const CRED_VARS: &'static [&'static str] = &[
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "AZURE_STORAGE_ACCOUNT",
        ];

        fn save_and_clear() -> Self {
            let saved: Vec<_> = Self::CRED_VARS
                .iter()
                .map(|k| (k.to_string(), std::env::var(k).ok()))
                .collect();
            for k in Self::CRED_VARS {
                unsafe { std::env::remove_var(k) };
            }
            Self { saved }
        }
    }

    impl Drop for CredentialEnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => unsafe { std::env::set_var(k, val) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }
}
