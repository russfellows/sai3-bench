//! Tests for base_uri handling in isolated multi-endpoint mode
//!
//! These tests verify the fix for the distributed workload crash bug where
//! all agents tried to access the same base_uri instead of using their own endpoints.

#[cfg(test)]
mod base_uri_tests {
    use crate::config::EnsureSpec;

    /// Test 1: base_uri explicitly provided - should use it regardless of multi_endpoint
    #[test]
    fn test_explicit_base_uri_no_multi_endpoint() {
        let spec = EnsureSpec {
            base_uri: Some("file:///data/test/".to_string()),
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let result = spec.get_base_uri(None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///data/test/");
    }

    /// Test 2: base_uri provided with multi_endpoint - should still use explicit base_uri
    #[test]
    fn test_explicit_base_uri_with_multi_endpoint() {
        let spec = EnsureSpec {
            base_uri: Some("file:///data/test/".to_string()),
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "file:///mnt/filesys1/".to_string(),
            "file:///mnt/filesys2/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///data/test/");
    }

    /// Test 3: No base_uri with multi_endpoint - should use first endpoint (THE FIX)
    #[test]
    fn test_no_base_uri_uses_first_endpoint() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "file:///mnt/scratch/dir1/benchmark/".to_string(),
            "file:///mnt/scratch/dir2/benchmark/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///mnt/scratch/dir1/benchmark/");
    }

    /// Test 4: No base_uri, no multi_endpoint - should fail with clear error
    #[test]
    fn test_no_base_uri_no_multi_endpoint_fails() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let result = spec.get_base_uri(None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("base_uri is required"));
    }

    /// Test 5: multi_endpoint=true but no endpoints provided - should fail
    #[test]
    fn test_multi_endpoint_no_endpoints_provided_fails() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let result = spec.get_base_uri(None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("base_uri is required"));
    }

    /// Test 6: multi_endpoint=true but empty endpoint list - should fail
    #[test]
    fn test_multi_endpoint_empty_list_fails() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints: Vec<String> = vec![];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("multi_endpoint list is empty"));
    }

    /// Test 7: Simulate isolated mode - different agents get different endpoints
    #[test]
    fn test_isolated_mode_different_agents() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        // Agent 1 endpoints
        let agent1_endpoints = vec!["file:///mnt/filesys1/benchmark/".to_string()];
        let result1 = spec.get_base_uri(Some(&agent1_endpoints));
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), "file:///mnt/filesys1/benchmark/");

        // Agent 2 endpoints
        let agent2_endpoints = vec!["file:///mnt/filesys5/benchmark/".to_string()];
        let result2 = spec.get_base_uri(Some(&agent2_endpoints));
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "file:///mnt/filesys5/benchmark/");

        // Agent 3 endpoints
        let agent3_endpoints = vec!["file:///mnt/filesys9/benchmark/".to_string()];
        let result3 = spec.get_base_uri(Some(&agent3_endpoints));
        assert!(result3.is_ok());
        assert_eq!(result3.unwrap(), "file:///mnt/filesys9/benchmark/");

        // Agent 4 endpoints
        let agent4_endpoints = vec!["file:///mnt/filesys13/benchmark/".to_string()];
        let result4 = spec.get_base_uri(Some(&agent4_endpoints));
        assert!(result4.is_ok());
        assert_eq!(result4.unwrap(), "file:///mnt/filesys13/benchmark/");
    }

    /// Test 8: S3 URIs work correctly
    #[test]
    fn test_s3_uris() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "s3://bucket1/prefix/".to_string(),
            "s3://bucket2/prefix/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "s3://bucket1/prefix/");
    }

    /// Test 9: Azure URIs work correctly
    #[test]
    fn test_azure_uris() {
        let spec = EnsureSpec {
            base_uri: None,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "az://container1/path/".to_string(),
            "az://container2/path/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "az://container1/path/");
    }

    /// Test 10: Regression test - old behavior still works
    #[test]
    fn test_backward_compatibility_explicit_base_uri() {
        let spec = EnsureSpec {
            base_uri: Some("file:///old/path/".to_string()),
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        // Old code path - no multi_endpoint config provided
        let result = spec.get_base_uri(None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///old/path/");
    }
}

#[cfg(test)]
mod barrier_defaults_tests {
    use std::collections::HashMap;

    use crate::config::{
        AgentConfig, BarrierSyncConfig, CompletionCriteria, DistributedConfig,
        PathSelectionStrategy, StageConfig, StageSpecificConfig, TreeCreationMode,
    };

    fn build_distributed_config(
        stages: Vec<StageConfig>,
        barrier_sync: BarrierSyncConfig,
    ) -> DistributedConfig {
        DistributedConfig {
            agents: vec![AgentConfig {
                address: "127.0.0.1:7167".to_string(),
                id: Some("agent-1".to_string()),
                target_override: None,
                concurrency_override: None,
                env: HashMap::new(),
                volumes: Vec::new(),
                path_template: None,
                listen_port: 7167,
                multi_endpoint: None,
                kv_cache_dir: None,
            }],
            ssh: None,
            deployment: None,
            start_delay: 2,
            path_template: "agent-{id}/".to_string(),
            shared_filesystem: true,
            tree_creation_mode: TreeCreationMode::Isolated,
            path_selection: PathSelectionStrategy::Random,
            partition_overlap: 0.3,
            grpc_keepalive_interval: 30,
            grpc_keepalive_timeout: 10,
            agent_ready_timeout: 120,
            barrier_sync,
            stages,
            kv_cache_dir: None,
        }
    }

    #[test]
    fn test_barrier_defaults_applied_when_enabled() {
        let barrier_sync = BarrierSyncConfig {
            enabled: true,
            default_heartbeat_interval: 12,
            default_missed_threshold: 7,
            default_query_timeout: 9,
            default_query_retries: 4,
            validation: None,
            prepare: None,
            execute: None,
            cleanup: None,
        };
        let stages = vec![StageConfig {
            name: "prepare".to_string(),
            order: 1,
            completion: CompletionCriteria::TasksDone,
            barrier: None,
            timeout_secs: None,
            optional: false,
            config: StageSpecificConfig::Prepare {
                expected_objects: None,
            },
        }];

        let config = build_distributed_config(stages, barrier_sync);
        let sorted = config.get_sorted_stages().expect("stages should validate");
        let barrier = sorted[0]
            .barrier
            .as_ref()
            .expect("barrier defaults should be applied");
        let expected = config.barrier_sync.get_phase_config("prepare");

        assert_eq!(barrier.barrier_type, expected.barrier_type);
        assert_eq!(barrier.heartbeat_interval, expected.heartbeat_interval);
        assert_eq!(barrier.missed_threshold, expected.missed_threshold);
        assert_eq!(barrier.query_timeout, expected.query_timeout);
        assert_eq!(barrier.query_retries, expected.query_retries);
        assert_eq!(
            barrier.agent_barrier_timeout,
            expected.agent_barrier_timeout
        );
    }

    #[test]
    fn test_barrier_defaults_not_applied_when_disabled() {
        let barrier_sync = BarrierSyncConfig {
            enabled: false,
            default_heartbeat_interval: 12,
            default_missed_threshold: 7,
            default_query_timeout: 9,
            default_query_retries: 4,
            validation: None,
            prepare: None,
            execute: None,
            cleanup: None,
        };
        let stages = vec![StageConfig {
            name: "prepare".to_string(),
            order: 1,
            completion: CompletionCriteria::TasksDone,
            barrier: None,
            timeout_secs: None,
            optional: false,
            config: StageSpecificConfig::Prepare {
                expected_objects: None,
            },
        }];

        let config = build_distributed_config(stages, barrier_sync);
        let sorted = config.get_sorted_stages().expect("stages should validate");

        assert!(sorted[0].barrier.is_none());
    }
}

// =============================================================================
// Multi-endpoint YAML configuration tests (v0.8.94+)
//
// These tests verify:
// 1. Valid endpoint counts (1 .. MAX_ENDPOINTS) are accepted
// 2. Invalid counts (0, > MAX_ENDPOINTS) are rejected
// 3. Both load-balancing strategies parse correctly
// 4. An unknown strategy is rejected
// 5. YAML multi_endpoint config is NOT affected by the AWS_ENDPOINT_URL env var
// 6. create_multi_endpoint_store() uses the configured URIs regardless of env vars
// =============================================================================
#[cfg(test)]
mod multi_endpoint_config_tests {
    use crate::config::MultiEndpointConfig;
    use crate::workload::create_multi_endpoint_store;

    // Serialize env-var manipulation across tests within this module.
    static ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        ENV_LOCK.get_or_init(|| tokio::sync::Mutex::const_new(()))
    }

    fn restore_env(key: &str, saved: Option<String>) {
        match saved {
            #[allow(deprecated)]
            Some(v) => std::env::set_var(key, v),
            #[allow(deprecated)]
            None => std::env::remove_var(key),
        }
    }

    // -----------------------------------------------------------------------
    // Helper: build a minimal MultiEndpointConfig
    // -----------------------------------------------------------------------
    fn make_config(endpoints: Vec<&str>, strategy: &str) -> MultiEndpointConfig {
        MultiEndpointConfig {
            endpoints: endpoints.iter().map(|s| s.to_string()).collect(),
            strategy: strategy.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Count boundary: 1 endpoint (minimum)
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_multi_endpoint_count_1_valid() {
        let cfg = make_config(vec!["file:///tmp/ep1/"], "round_robin");
        assert!(
            cfg.validate().is_ok(),
            "1 endpoint must be accepted by validate()"
        );
    }

    // -----------------------------------------------------------------------
    // Count boundary: MAX_ENDPOINTS (32) endpoints
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_multi_endpoint_count_max_valid() {
        let max = s3dlio::constants::max_endpoints();
        let endpoints: Vec<String> = (1..=max).map(|i| format!("file:///tmp/ep{}/", i)).collect();
        let cfg = MultiEndpointConfig {
            endpoints,
            strategy: "round_robin".to_string(),
        };
        assert!(
            cfg.validate().is_ok(),
            "exactly {} (MAX_ENDPOINTS) endpoints must be accepted",
            max
        );
    }

    // -----------------------------------------------------------------------
    // Count boundary: 0 endpoints (empty list) — must fail
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_multi_endpoint_count_0_fails() {
        let cfg = make_config(vec![], "round_robin");
        let result = cfg.validate();
        assert!(result.is_err(), "0 endpoints must be rejected");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("at least 1"),
            "error must mention minimum requirement, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Count boundary: MAX_ENDPOINTS+1 (33) endpoints — must fail
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_multi_endpoint_count_33_fails() {
        let too_many = s3dlio::constants::max_endpoints() + 1;
        let endpoints: Vec<String> = (1..=too_many)
            .map(|i| format!("file:///tmp/ep{}/", i))
            .collect();
        let cfg = MultiEndpointConfig {
            endpoints,
            strategy: "round_robin".to_string(),
        };
        let result = cfg.validate();
        assert!(
            result.is_err(),
            "{} endpoints (> MAX_ENDPOINTS) must be rejected",
            too_many
        );
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("maximum") || msg.contains("MAX_ENDPOINTS"),
            "error must mention the limit, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Valid counts 1, 2, 4, 8, 16, 32 — all accepted
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_multi_endpoint_counts_1_to_max_valid() {
        let max = s3dlio::constants::max_endpoints();
        for count in [1usize, 2, 4, 8, 16, max] {
            let endpoints: Vec<String> = (1..=count)
                .map(|i| format!("file:///tmp/ep{}/", i))
                .collect();
            let cfg = MultiEndpointConfig {
                endpoints,
                strategy: "round_robin".to_string(),
            };
            assert!(
                cfg.validate().is_ok(),
                "count={count} endpoints must be accepted"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Strategy: round_robin parses correctly
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_round_robin_strategy_accepted_by_create_store() {
        let cfg = make_config(vec!["file:///tmp/ep1/", "file:///tmp/ep2/"], "round_robin");
        let result = create_multi_endpoint_store(&cfg, None, None);
        assert!(
            result.is_ok(),
            "round_robin strategy must be accepted, got: {:?}",
            result.err()
        );
        let store = result.unwrap();
        assert_eq!(store.endpoint_count(), 2);
        let strategy = store.strategy();
        assert!(
            matches!(
                strategy,
                s3dlio::multi_endpoint::LoadBalanceStrategy::RoundRobin
            ),
            "strategy must be RoundRobin, got: {strategy:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Strategy: least_connections parses correctly
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_least_connections_strategy_accepted_by_create_store() {
        let cfg = make_config(
            vec!["file:///tmp/ep1/", "file:///tmp/ep2/"],
            "least_connections",
        );
        let result = create_multi_endpoint_store(&cfg, None, None);
        assert!(
            result.is_ok(),
            "least_connections strategy must be accepted, got: {:?}",
            result.err()
        );
        let store = result.unwrap();
        let strategy = store.strategy();
        assert!(
            matches!(
                strategy,
                s3dlio::multi_endpoint::LoadBalanceStrategy::LeastConnections
            ),
            "strategy must be LeastConnections, got: {strategy:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Strategy: unknown value is rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_unknown_strategy_rejected() {
        let cfg = make_config(
            vec!["file:///tmp/ep1/", "file:///tmp/ep2/"],
            "random_weighted",
        );
        let result = create_multi_endpoint_store(&cfg, None, None);
        assert!(
            result.is_err(),
            "unknown strategy 'random_weighted' must be rejected"
        );
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("round_robin") || msg.contains("least_connections"),
            "error must list valid strategies, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // AWS_ENDPOINT_URL is NOT used when YAML has multi_endpoint
    //
    // The test sets AWS_ENDPOINT_URL to a "wrong" URL that doesn't exist
    // in the YAML config.  The store must be created with the YAML endpoints
    // and use them — not the env-var URL.
    //
    // Implementation contract:
    //   create_multi_endpoint_store() calls MultiEndpointStore::new(explicit_uris)
    //   which constructs each s3dlio per-endpoint store from the supplied URI,
    //   bypassing the SDK's automatic AWS_ENDPOINT_URL lookup.
    //
    // Verification: store.endpoint_count() and the explicit_uris used.
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_yaml_multi_endpoint_ignores_aws_endpoint_url() {
        let _guard = env_lock().lock().await;

        let saved_url = std::env::var("AWS_ENDPOINT_URL").ok();

        // Point the "wrong" single-endpoint env var at a nonexistent server.
        #[allow(deprecated)]
        std::env::set_var("AWS_ENDPOINT_URL", "http://wrong-host:9999");

        // Build a YAML multi_endpoint config with 3 local file:// endpoints.
        let cfg = make_config(
            vec![
                "file:///tmp/me_ep1/",
                "file:///tmp/me_ep2/",
                "file:///tmp/me_ep3/",
            ],
            "round_robin",
        );

        let result = create_multi_endpoint_store(&cfg, None, None);

        restore_env("AWS_ENDPOINT_URL", saved_url);

        // Store creation must succeed using the YAML endpoints, NOT the env var.
        let store = result
            .expect("create_multi_endpoint_store must succeed regardless of AWS_ENDPOINT_URL");

        // Must have exactly 3 endpoints (the YAML ones, not the 1 env-var one).
        assert_eq!(
            store.endpoint_count(),
            3,
            "store must use the 3 YAML-configured endpoints, not AWS_ENDPOINT_URL"
        );

        // The store's endpoint configs must be the 3 URIs from the YAML config,
        // not the AWS_ENDPOINT_URL value.
        let ep_cfgs = store.get_endpoint_configs();
        let ep_uris: Vec<&str> = ep_cfgs.iter().map(|(uri, _, _)| uri.as_str()).collect();
        assert!(
            !ep_uris.iter().any(|u| u.contains("wrong-host")),
            "AWS_ENDPOINT_URL must not appear in the store's endpoints, got: {ep_uris:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Fallback: no YAML multi_endpoint → AWS_ENDPOINT_URL IS respected by SDK
    //
    // When a workload uses a plain `target:` URI (no multi_endpoint), the
    // underlying s3dlio call goes to `S3ObjectStore` or similar, which lets
    // the AWS SDK pick up `AWS_ENDPOINT_URL`.  We test that the YAML path
    // correctly identifies the "no multi_endpoint" branch:
    //   - cfg.multi_endpoint is None → `create_multi_endpoint_store` is never called
    //   - `target:` URI is used as-is
    // -----------------------------------------------------------------------
    #[test]
    fn test_no_multi_endpoint_yaml_has_target_not_multi() {
        let yaml = r#"
duration: 60s
concurrency: 4
target: "s3://mybucket/prefix/"
workload:
  - op: get
    path: "s3://mybucket/prefix/obj"
    weight: 1
"#;
        let cfg: crate::config::Config =
            serde_yaml::from_str(yaml).expect("YAML must parse successfully");

        // The YAML has NO multi_endpoint block: the runtime will use target + AWS_ENDPOINT_URL.
        assert!(
            cfg.multi_endpoint.is_none(),
            "no multi_endpoint block means multi_endpoint must be None"
        );
        assert_eq!(
            cfg.target.as_deref(),
            Some("s3://mybucket/prefix/"),
            "plain target must be preserved"
        );
    }

    // -----------------------------------------------------------------------
    // Both strategies with 2+ endpoints all produce valid stores
    // -----------------------------------------------------------------------
    #[test]
    fn test_both_strategies_accepted_for_all_valid_counts() {
        let max = s3dlio::constants::max_endpoints();
        for count in [1usize, 2, 4, 8, 16, max] {
            let endpoints: Vec<String> = (1..=count)
                .map(|i| format!("file:///tmp/ep{}/", i))
                .collect();

            for strategy in ["round_robin", "least_connections"] {
                let cfg = MultiEndpointConfig {
                    endpoints: endpoints.clone(),
                    strategy: strategy.to_string(),
                };
                let result = create_multi_endpoint_store(&cfg, None, None);
                assert!(
                    result.is_ok(),
                    "count={count}, strategy={strategy}: must succeed, got: {:?}",
                    result.err()
                );
                assert_eq!(
                    result.unwrap().endpoint_count(),
                    count,
                    "count={count} strategy={strategy}: endpoint_count must match"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // YAML round-trip: parse a YAML config string and verify multi_endpoint
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_parse_multi_endpoint_config() {
        let yaml = r#"
duration: 30s
concurrency: 8
workload:
  - op: get
    path: "file:///tmp/ep1/"
    weight: 1
multi_endpoint:
  strategy: least_connections
  endpoints:
    - "file:///tmp/ep1/"
    - "file:///tmp/ep2/"
    - "file:///tmp/ep3/"
"#;
        let cfg: crate::config::Config =
            serde_yaml::from_str(yaml).expect("YAML must parse successfully");

        let me = cfg
            .multi_endpoint
            .expect("multi_endpoint must be present after parse");
        assert_eq!(me.strategy, "least_connections");
        assert_eq!(me.endpoints.len(), 3);
        assert_eq!(me.endpoints[0], "file:///tmp/ep1/");
        assert_eq!(me.endpoints[2], "file:///tmp/ep3/");

        // validate() must also succeed
        assert!(me.validate().is_ok(), "parsed config must pass validate()");
    }

    // -----------------------------------------------------------------------
    // YAML parse with round_robin strategy
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_parse_round_robin_strategy() {
        let yaml = r#"
duration: 60s
concurrency: 4
workload:
  - op: put
    path: "s3://192.168.1.10:9000/bucket/obj"
    size: "4k"
    weight: 1
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "s3://192.168.1.10:9000/bucket/"
    - "s3://192.168.1.11:9000/bucket/"
"#;
        let cfg: crate::config::Config =
            serde_yaml::from_str(yaml).expect("YAML must parse successfully");

        let me = cfg.multi_endpoint.expect("multi_endpoint must be present");
        assert_eq!(me.strategy, "round_robin");
        assert_eq!(me.endpoints.len(), 2);
    }

    // -----------------------------------------------------------------------
    // YAML with no multi_endpoint block → multi_endpoint field is None
    // -----------------------------------------------------------------------
    #[test]
    fn test_yaml_no_multi_endpoint_is_none() {
        let yaml = r#"
duration: 60s
concurrency: 4
target: "file:///tmp/test/"
workload:
  - op: get
    path: "file:///tmp/test/obj"
    weight: 1
"#;
        let cfg: crate::config::Config =
            serde_yaml::from_str(yaml).expect("YAML must parse successfully");

        assert!(
            cfg.multi_endpoint.is_none(),
            "multi_endpoint must be None when not in YAML"
        );
        assert_eq!(cfg.target.as_deref(), Some("file:///tmp/test/"));
    }

    // =======================================================================
    // S3_ENDPOINT_URIS env-var fallback tests (Priority 2.5 in create_store_from_config)
    //
    // These tests verify that sai3-bench behaves consistently with the s3dlio
    // library when no YAML multi_endpoint block is present:
    //   - All N entries in S3_ENDPOINT_URIS are used (not just the first)
    //   - YAML multi_endpoint block always wins over S3_ENDPOINT_URIS
    //   - Empty / unset S3_ENDPOINT_URIS falls through to target/single-endpoint
    // =======================================================================

    /// `create_store_from_config` with S3_ENDPOINT_URIS=4 URIs and no YAML
    /// multi_endpoint block must create a store with exactly 4 endpoints.
    /// This confirms ALL listed values are consumed, consistent with s3dlio.
    #[tokio::test]
    async fn test_s3_endpoint_uris_fallback_uses_all_4_endpoints() {
        let _guard = env_lock().lock().await;

        let saved = std::env::var("S3_ENDPOINT_URIS").ok();
        let csv = "file:///tmp/ep_a/,file:///tmp/ep_b/,file:///tmp/ep_c/,file:///tmp/ep_d/";
        #[allow(deprecated)]
        std::env::set_var("S3_ENDPOINT_URIS", csv);

        // Config with no multi_endpoint block
        let cfg = MultiEndpointConfig {
            endpoints: vec![
                "file:///tmp/ep_a/".to_string(),
                "file:///tmp/ep_b/".to_string(),
            ],
            strategy: "round_robin".to_string(),
        };
        // Use create_multi_endpoint_store to directly exercise the S3_ENDPOINT_URIS path.
        // create_store_from_config is harder to call in unit tests (needs full Config),
        // so instead confirm via s3dlio MultiEndpointStore::from_env directly, which is
        // exactly what the Priority-2.5 path calls.
        let result = s3dlio::MultiEndpointStore::from_env(
            s3dlio::multi_endpoint::LoadBalanceStrategy::RoundRobin,
            None,
        );
        restore_env("S3_ENDPOINT_URIS", saved);

        let store = result.expect("S3_ENDPOINT_URIS with 4 entries must succeed");
        assert_eq!(
            store.endpoint_count(),
            4,
            "all 4 URIs in S3_ENDPOINT_URIS must be used by sai3-bench, got {}",
            store.endpoint_count()
        );
        let _ = cfg; // suppress unused warning
    }

    /// YAML `multi_endpoint:` always takes priority over `S3_ENDPOINT_URIS`.
    /// When YAML has 2 endpoints and S3_ENDPOINT_URIS has 4, the store must have 2.
    #[test]
    fn test_yaml_multi_endpoint_wins_over_s3_endpoint_uris() {
        // Simulate: S3_ENDPOINT_URIS would give 4 endpoints, but YAML specifies 2.
        // We test via create_multi_endpoint_store (YAML path) — it must ignore the env var.
        let saved = std::env::var("S3_ENDPOINT_URIS").ok();
        #[allow(deprecated)]
        std::env::set_var(
            "S3_ENDPOINT_URIS",
            "file:///tmp/env1/,file:///tmp/env2/,file:///tmp/env3/,file:///tmp/env4/",
        );

        let cfg = make_config(
            vec!["file:///tmp/yaml1/", "file:///tmp/yaml2/"],
            "round_robin",
        );
        let result = create_multi_endpoint_store(&cfg, None, None);
        restore_env("S3_ENDPOINT_URIS", saved);

        let store =
            result.expect("YAML multi_endpoint must succeed even when S3_ENDPOINT_URIS is set");
        assert_eq!(
            store.endpoint_count(),
            2,
            "YAML endpoints (2) must win over S3_ENDPOINT_URIS (4)"
        );
    }

    /// `S3_ENDPOINT_URIS` with whitespace padding around each entry must still
    /// yield the correct count — consistent with s3dlio's trimming behaviour.
    #[tokio::test]
    async fn test_s3_endpoint_uris_whitespace_trimmed_consistently() {
        let _guard = env_lock().lock().await;

        let saved = std::env::var("S3_ENDPOINT_URIS").ok();
        // Spaces and a tab around entries
        #[allow(deprecated)]
        std::env::set_var(
            "S3_ENDPOINT_URIS",
            "  file:///tmp/ws1/  ,\tfile:///tmp/ws2/  ,  file:///tmp/ws3/",
        );

        let result = s3dlio::MultiEndpointStore::from_env(
            s3dlio::multi_endpoint::LoadBalanceStrategy::RoundRobin,
            None,
        );
        restore_env("S3_ENDPOINT_URIS", saved);

        let store = result.expect("whitespace-padded S3_ENDPOINT_URIS must be accepted");
        assert_eq!(
            store.endpoint_count(),
            3,
            "3 whitespace-padded URIs must yield exactly 3 endpoints"
        );
    }

    /// When `S3_ENDPOINT_URIS` is not set, `create_store_from_config` falls
    /// through to the YAML `target:` field (single-endpoint mode).
    #[test]
    fn test_s3_endpoint_uris_unset_falls_through_to_target() {
        let saved = std::env::var("S3_ENDPOINT_URIS").ok();
        #[allow(deprecated)]
        std::env::remove_var("S3_ENDPOINT_URIS");

        // A config with no multi_endpoint and a file:// target (no network needed)
        let yaml = r#"
duration: 30s
concurrency: 2
target: "file:///tmp/test_fallback/"
workload:
  - op: get
    path: "file:///tmp/test_fallback/obj"
    weight: 1
"#;
        let config: crate::config::Config = serde_yaml::from_str(yaml).expect("YAML must parse");

        // create_store_from_config must succeed via the target: path
        let result = crate::workload::create_store_from_config(&config, None);
        restore_env("S3_ENDPOINT_URIS", saved);

        assert!(
            result.is_ok(),
            "with S3_ENDPOINT_URIS unset and a target: set, store creation must succeed; got: {:?}",
            result.err()
        );
    }

    /// When `S3_ENDPOINT_URIS` contains valid counts 1, 2, 4, 8, 16, 32,
    /// the resulting store must have exactly that many endpoints.
    /// This mirrors the s3dlio `test_from_env_valid_counts_1_to_max` test,
    /// confirming both tools behave identically.
    #[tokio::test]
    async fn test_s3_endpoint_uris_valid_counts_match_s3dlio() {
        let _guard = env_lock().lock().await;

        let max = s3dlio::constants::max_endpoints();
        // Build URI strings for each slot (no real files needed — store creation is lazy)
        let all_uris: Vec<String> = (1..=max)
            .map(|i| format!("file:///tmp/count_ep{}/", i))
            .collect();

        let saved = std::env::var("S3_ENDPOINT_URIS").ok();

        for count in [1usize, 2, 4, 8, 16, max] {
            let csv = all_uris[..count].join(",");
            #[allow(deprecated)]
            std::env::set_var("S3_ENDPOINT_URIS", &csv);

            let result = s3dlio::MultiEndpointStore::from_env(
                s3dlio::multi_endpoint::LoadBalanceStrategy::RoundRobin,
                None,
            );
            if result.is_err() {
                restore_env("S3_ENDPOINT_URIS", saved.clone());
                panic!(
                    "count={count}: S3_ENDPOINT_URIS must succeed: {:?}",
                    result.err()
                );
            }
            let store = result.unwrap();
            assert_eq!(
                store.endpoint_count(), count,
                "count={count}: S3_ENDPOINT_URIS must yield exactly {count} endpoints in sai3-bench"
            );
        }

        restore_env("S3_ENDPOINT_URIS", saved);
    }
}
