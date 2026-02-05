// tests/distributed_config_tests.rs
//! Tests for distributed configuration parsing and validation

use anyhow::Result;
use sai3_bench::config::{Config, TreeCreationMode};

#[test]
fn test_parse_distributed_config_basic() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s
concurrency: 32

distributed:
  agents:
    - address: "localhost:7761"
      id: "agent-1"
    
    - address: "localhost:7762"
      id: "agent-2"
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.distributed.is_some());
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.agents.len(), 2);
    assert_eq!(dist.agents[0].address, "localhost:7761");
    assert_eq!(dist.agents[0].id, Some("agent-1".to_string()));
    assert_eq!(dist.agents[1].address, "localhost:7762");
    
    Ok(())
}

#[test]
fn test_parse_distributed_config_with_overrides() -> Result<()> {
    let yaml = r#"
target: "s3://default-bucket/"
duration: 60s
concurrency: 64

distributed:
  agents:
    - address: "vm1.example.com"
      id: "reader"
      env:
        AWS_PROFILE: "benchmark-reader"
        RUST_LOG: "info"
    
    - address: "vm2.example.com"
      id: "writer"
      target_override: "s3://bucket-2/"
      concurrency_override: 128
      env:
        AWS_PROFILE: "benchmark-writer"
      volumes:
        - "/mnt/data:/data"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: random

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    let dist = config.distributed.as_ref().unwrap();
    
    // Check first agent
    assert_eq!(dist.agents[0].env.get("AWS_PROFILE"), Some(&"benchmark-reader".to_string()));
    assert_eq!(dist.agents[0].env.get("RUST_LOG"), Some(&"info".to_string()));
    assert!(dist.agents[0].target_override.is_none());
    
    // Check second agent with overrides
    assert_eq!(dist.agents[1].target_override, Some("s3://bucket-2/".to_string()));
    assert_eq!(dist.agents[1].concurrency_override, Some(128));
    assert_eq!(dist.agents[1].volumes.len(), 1);
    assert_eq!(dist.agents[1].volumes[0], "/mnt/data:/data");
    
    Ok(())
}

#[test]
fn test_parse_ssh_config() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "vm1.example.com"
  
  ssh:
    enabled: true
    user: "ubuntu"
    key_path: "~/.ssh/test_key"
    timeout: 15
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    let dist = config.distributed.as_ref().unwrap();
    let ssh = dist.ssh.as_ref().unwrap();
    
    assert!(ssh.enabled);
    assert_eq!(ssh.user, Some("ubuntu".to_string()));
    assert_eq!(ssh.key_path, Some("~/.ssh/test_key".to_string()));
    assert_eq!(ssh.timeout, 15);
    
    Ok(())
}

#[test]
fn test_parse_deployment_config() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  deployment:
    deploy_type: "docker"
    image: "sai3bench:v0.6.11"
    network_mode: "host"
    pull_policy: "always"
    docker_args:
      - "--cpus=2"
      - "--memory=4g"
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    let dist = config.distributed.as_ref().unwrap();
    let deploy = dist.deployment.as_ref().unwrap();
    
    assert_eq!(deploy.deploy_type, "docker");
    assert_eq!(deploy.image, "sai3bench:v0.6.11");
    assert_eq!(deploy.network_mode, "host");
    assert_eq!(deploy.pull_policy, "always");
    assert_eq!(deploy.docker_args.len(), 2);
    assert!(deploy.docker_args.contains(&"--cpus=2".to_string()));
    
    Ok(())
}

#[test]
fn test_parse_distributed_defaults() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    let dist = config.distributed.as_ref().unwrap();
    
    // Test defaults
    assert_eq!(dist.start_delay, 2);  // default_start_delay
    assert_eq!(dist.path_template, "agent-{id}/");  // default_path_template
    assert_eq!(dist.agents[0].listen_port, 7761);  // default_agent_port
    assert!(dist.agents[0].env.is_empty());
    assert!(dist.agents[0].volumes.is_empty());
    
    Ok(())
}

#[test]
fn test_backward_compatibility_no_distributed() -> Result<()> {
    // Config without distributed section should still parse
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s
concurrency: 32

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.distributed.is_none());
    assert_eq!(config.concurrency, 32);
    
    Ok(())
}

#[test]
fn test_parse_real_config_file() -> Result<()> {
    // Parse the actual test config file
    let config_path = "tests/configs/local_test_2agents.yaml";
    let yaml = std::fs::read_to_string(config_path)?;
    let config: Config = serde_yaml::from_str(&yaml)?;
    
    assert!(config.distributed.is_some());
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.agents.len(), 2);
    assert_eq!(dist.agents[0].address, "127.0.0.1:7761");
    assert_eq!(dist.agents[0].id, Some("test-agent-1".to_string()));
    
    // Verify shared filesystem settings
    assert!(dist.shared_filesystem);
    assert_eq!(dist.tree_creation_mode, TreeCreationMode::Coordinator);
    
    Ok(())
}

#[test]
fn test_agent_address_port_parsing() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "vm1.example.com:7761"  # Explicit port
      id: "agent-1"
    
    - address: "vm2.example.com"  # No port, should use listen_port
      id: "agent-2"
      listen_port: 8888
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.agents[0].address, "vm1.example.com:7761");
    assert_eq!(dist.agents[1].address, "vm2.example.com");
    assert_eq!(dist.agents[1].listen_port, 8888);
    
    Ok(())
}

#[test]
fn test_example_config_parsing() -> Result<()> {
    // Parse the comprehensive example config
    let config_path = "examples/distributed-ssh-automated.yaml";
    
    if std::path::Path::new(config_path).exists() {
        let yaml = std::fs::read_to_string(config_path)?;
        let config: Config = serde_yaml::from_str(&yaml)?;
        
        assert!(config.distributed.is_some());
        let dist = config.distributed.as_ref().unwrap();
        
        // Should have 3 agents in example
        assert_eq!(dist.agents.len(), 3);
        
        // SSH should be enabled
        assert!(dist.ssh.as_ref().unwrap().enabled);
        
        // Deployment config should exist
        assert!(dist.deployment.is_some());
        
        println!("✓ Example config parses successfully");
    } else {
        println!("⚠ Skipping - example config not found");
    }
    
    Ok(())
}

#[test]
fn test_invalid_yaml_errors() {
    // Missing required fields should fail
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - id: "no-address"  # Missing required 'address' field

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let result: Result<Config, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err(), "Should fail when agent address is missing");
}

#[test]
fn test_env_vars_parsing() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
      env:
        AWS_ACCESS_KEY_ID: "test-key"
        AWS_SECRET_ACCESS_KEY: "test-secret"
        AWS_REGION: "us-west-2"
        RUST_LOG: "debug"
        CUSTOM_VAR: "custom-value"
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    let env = &dist.agents[0].env;
    
    assert_eq!(env.len(), 5);
    assert_eq!(env.get("AWS_ACCESS_KEY_ID"), Some(&"test-key".to_string()));
    assert_eq!(env.get("AWS_REGION"), Some(&"us-west-2".to_string()));
    assert_eq!(env.get("CUSTOM_VAR"), Some(&"custom-value".to_string()));
    
    Ok(())
}

// ============================================================================
// Tests for TreeCreationMode and PathSelectionStrategy (v0.7.0+)
// ============================================================================

#[test]
fn test_parse_tree_creation_mode_isolated() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
      id: "agent-1"
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert!(!dist.shared_filesystem);
    assert_eq!(dist.tree_creation_mode, sai3_bench::config::TreeCreationMode::Isolated);
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Random);
    
    Ok(())
}

#[test]
fn test_parse_tree_creation_mode_coordinator() -> Result<()> {
    let yaml = r#"
target: "s3://shared-bucket/"
duration: 60s

distributed:
  agents:
    - address: "vm1:7761"
    - address: "vm2:7761"
  
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: partitioned

workload:
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert!(dist.shared_filesystem);
    assert_eq!(dist.tree_creation_mode, sai3_bench::config::TreeCreationMode::Coordinator);
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Partitioned);
    
    Ok(())
}

#[test]
fn test_parse_tree_creation_mode_concurrent() -> Result<()> {
    let yaml = r#"
target: "az://container/"
duration: 30s

distributed:
  agents:
    - address: "localhost:7761"
    - address: "localhost:7762"
    - address: "localhost:7763"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: exclusive

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert!(dist.shared_filesystem);
    assert_eq!(dist.tree_creation_mode, sai3_bench::config::TreeCreationMode::Concurrent);
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Exclusive);
    
    Ok(())
}

#[test]
fn test_parse_path_selection_strategy_random() -> Result<()> {
    let yaml = r#"
target: "file:///mnt/nfs/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Random);
    
    Ok(())
}

#[test]
fn test_parse_path_selection_strategy_partitioned() -> Result<()> {
    let yaml = r#"
target: "s3://bucket/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: partitioned
  partition_overlap: 0.2

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Partitioned);
    assert_eq!(dist.partition_overlap, 0.2);
    
    Ok(())
}

#[test]
fn test_parse_path_selection_strategy_exclusive() -> Result<()> {
    let yaml = r#"
target: "gs://bucket/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: exclusive

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Exclusive);
    
    Ok(())
}

#[test]
fn test_parse_path_selection_strategy_weighted() -> Result<()> {
    let yaml = r#"
target: "file:///mnt/lustre/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: weighted
  partition_overlap: 0.5

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Weighted);
    assert_eq!(dist.partition_overlap, 0.5);
    
    Ok(())
}

#[test]
fn test_parse_partition_overlap_default() -> Result<()> {
    // When partition_overlap not specified, should use default (0.3)
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: partitioned

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Should use default_partition_overlap() = 0.3
    assert_eq!(dist.partition_overlap, 0.3);
    
    Ok(())
}

#[test]
fn test_parse_partition_overlap_zero() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: partitioned
  partition_overlap: 0.0

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.partition_overlap, 0.0);
    
    Ok(())
}

#[test]
fn test_parse_partition_overlap_one() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: weighted
  partition_overlap: 1.0

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.partition_overlap, 1.0);
    
    Ok(())
}

#[test]
fn test_shared_filesystem_true() -> Result<()> {
    let yaml = r#"
target: "s3://bucket/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert!(dist.shared_filesystem);
    
    Ok(())
}

#[test]
fn test_shared_filesystem_false() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert!(!dist.shared_filesystem);
    
    Ok(())
}

#[test]
fn test_enum_equality() {
    use sai3_bench::config::{TreeCreationMode, PathSelectionStrategy};
    
    // TreeCreationMode equality
    assert_eq!(TreeCreationMode::Isolated, TreeCreationMode::Isolated);
    assert_ne!(TreeCreationMode::Isolated, TreeCreationMode::Coordinator);
    assert_ne!(TreeCreationMode::Coordinator, TreeCreationMode::Concurrent);
    
    // PathSelectionStrategy equality
    assert_eq!(PathSelectionStrategy::Random, PathSelectionStrategy::Random);
    assert_ne!(PathSelectionStrategy::Random, PathSelectionStrategy::Partitioned);
    assert_ne!(PathSelectionStrategy::Partitioned, PathSelectionStrategy::Exclusive);
    assert_ne!(PathSelectionStrategy::Exclusive, PathSelectionStrategy::Weighted);
}

#[test]
fn test_enum_clone() {
    use sai3_bench::config::{TreeCreationMode, PathSelectionStrategy};
    
    let mode = TreeCreationMode::Concurrent;
    let mode_clone = mode.clone();
    assert_eq!(mode, mode_clone);
    
    let strategy = PathSelectionStrategy::Weighted;
    let strategy_clone = strategy.clone();
    assert_eq!(strategy, strategy_clone);
}

#[test]
fn test_enum_debug_format() {
    use sai3_bench::config::{TreeCreationMode, PathSelectionStrategy};
    
    // TreeCreationMode debug
    assert_eq!(format!("{:?}", TreeCreationMode::Isolated), "Isolated");
    assert_eq!(format!("{:?}", TreeCreationMode::Coordinator), "Coordinator");
    assert_eq!(format!("{:?}", TreeCreationMode::Concurrent), "Concurrent");
    
    // PathSelectionStrategy debug
    assert_eq!(format!("{:?}", PathSelectionStrategy::Random), "Random");
    assert_eq!(format!("{:?}", PathSelectionStrategy::Partitioned), "Partitioned");
    assert_eq!(format!("{:?}", PathSelectionStrategy::Exclusive), "Exclusive");
    assert_eq!(format!("{:?}", PathSelectionStrategy::Weighted), "Weighted");
}

#[test]
fn test_invalid_tree_creation_mode() {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: invalid_mode
  path_selection: random

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let result: Result<Config, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err(), "Should fail with invalid tree_creation_mode");
}

#[test]
fn test_invalid_path_selection_strategy() {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 10s

distributed:
  agents:
    - address: "localhost:7761"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: invalid_strategy

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let result: Result<Config, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err(), "Should fail with invalid path_selection");
}

#[test]
fn test_comprehensive_distributed_config_with_directory_tree() -> Result<()> {
    // Full example with all new fields
    let yaml = r#"
target: "s3://benchmark-bucket/"
duration: 300s
concurrency: 128

prepare:
  count: 10000
  min_size: 1048576
  max_size: 10485760

distributed:
  agents:
    - address: "vm1.example.com:7761"
      id: "agent-1"
      env:
        AWS_PROFILE: "benchmark"
    
    - address: "vm2.example.com:7761"
      id: "agent-2"
      env:
        AWS_PROFILE: "benchmark"
  
  shared_filesystem: true
  tree_creation_mode: concurrent
  path_selection: partitioned
  partition_overlap: 0.25
  
  start_delay: 3
  path_template: "node-{id}/"

workload:
  - op: get
    path: "data/*"
    weight: 70
  
  - op: put
    path: "data/"
    object_size: 2097152
    weight: 30
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    // Validate all new fields
    let dist = config.distributed.as_ref().unwrap();
    assert_eq!(dist.agents.len(), 2);
    assert!(dist.shared_filesystem);
    assert_eq!(dist.tree_creation_mode, sai3_bench::config::TreeCreationMode::Concurrent);
    assert_eq!(dist.path_selection, sai3_bench::config::PathSelectionStrategy::Partitioned);
    assert_eq!(dist.partition_overlap, 0.25);
    assert_eq!(dist.start_delay, 3);
    assert_eq!(dist.path_template, "node-{id}/");
    
    Ok(())
}

#[test]
fn test_serialize_deserialize_round_trip() -> Result<()> {
    use sai3_bench::config::{DistributedConfig, TreeCreationMode, PathSelectionStrategy, AgentConfig, BarrierSyncConfig};
    
    let original_dist = DistributedConfig {
        agents: vec![
            AgentConfig {
                address: "localhost:7761".to_string(),
                id: Some("test-agent".to_string()),
                target_override: None,
                concurrency_override: None,
                env: std::collections::HashMap::new(),
                volumes: vec![],
                path_template: None,
                listen_port: 7761,
                multi_endpoint: None,
            }
        ],
        ssh: None,
        deployment: None,
        start_delay: 2,
        path_template: "agent-{id}/".to_string(),
        shared_filesystem: true,
        tree_creation_mode: TreeCreationMode::Concurrent,
        path_selection: PathSelectionStrategy::Partitioned,
        partition_overlap: 0.3,
        barrier_sync: BarrierSyncConfig::default(),
        stages: None,
    };
    
    // Serialize to YAML
    let yaml = serde_yaml::to_string(&original_dist)?;
    
    // Deserialize back
    let deserialized: DistributedConfig = serde_yaml::from_str(&yaml)?;
    
    // Verify all fields match
    assert_eq!(deserialized.agents.len(), 1);
    assert!(deserialized.shared_filesystem);
    assert_eq!(deserialized.tree_creation_mode, TreeCreationMode::Concurrent);
    assert_eq!(deserialized.path_selection, PathSelectionStrategy::Partitioned);
    assert_eq!(deserialized.partition_overlap, 0.3);
    
    Ok(())
}

// =============================================================================
// ReplayConfig (Backpressure) Tests (v0.8.9+)
// =============================================================================

#[test]
fn test_replay_config_default() {
    use sai3_bench::config::ReplayConfig;
    use std::time::Duration;
    
    let config = ReplayConfig::default();
    
    assert_eq!(config.lag_threshold, Duration::from_secs(5));
    assert_eq!(config.recovery_threshold, Duration::from_secs(1));
    assert_eq!(config.max_flaps_per_minute, 3);
    assert_eq!(config.drain_timeout, Duration::from_secs(10));
    assert_eq!(config.max_concurrent, 1000);
}

#[test]
fn test_replay_config_parse_full() -> Result<()> {
    use sai3_bench::config::ReplayConfig;
    use std::time::Duration;
    
    let yaml = r#"
lag_threshold: 10s
recovery_threshold: 2s
max_flaps_per_minute: 5
drain_timeout: 30s
max_concurrent: 500
"#;

    let config: ReplayConfig = serde_yaml::from_str(yaml)?;
    
    assert_eq!(config.lag_threshold, Duration::from_secs(10));
    assert_eq!(config.recovery_threshold, Duration::from_secs(2));
    assert_eq!(config.max_flaps_per_minute, 5);
    assert_eq!(config.drain_timeout, Duration::from_secs(30));
    assert_eq!(config.max_concurrent, 500);
    
    Ok(())
}

#[test]
fn test_replay_config_parse_partial_with_defaults() -> Result<()> {
    use sai3_bench::config::ReplayConfig;
    use std::time::Duration;
    
    // Only specify some fields, others should use defaults
    let yaml = r#"
lag_threshold: 8s
max_concurrent: 2000
"#;

    let config: ReplayConfig = serde_yaml::from_str(yaml)?;
    
    assert_eq!(config.lag_threshold, Duration::from_secs(8));
    assert_eq!(config.recovery_threshold, Duration::from_secs(1)); // default
    assert_eq!(config.max_flaps_per_minute, 3); // default
    assert_eq!(config.drain_timeout, Duration::from_secs(10)); // default
    assert_eq!(config.max_concurrent, 2000);
    
    Ok(())
}

#[test]
fn test_replay_config_parse_humantime_durations() -> Result<()> {
    use sai3_bench::config::ReplayConfig;
    use std::time::Duration;
    
    let yaml = r#"
lag_threshold: 5000ms
recovery_threshold: 500ms
drain_timeout: 1m
"#;

    let config: ReplayConfig = serde_yaml::from_str(yaml)?;
    
    assert_eq!(config.lag_threshold, Duration::from_millis(5000));
    assert_eq!(config.recovery_threshold, Duration::from_millis(500));
    assert_eq!(config.drain_timeout, Duration::from_secs(60));
    
    Ok(())
}

#[test]
fn test_replay_config_serialize_deserialize() -> Result<()> {
    use sai3_bench::config::ReplayConfig;
    use std::time::Duration;
    
    let original = ReplayConfig {
        lag_threshold: Duration::from_secs(7),
        recovery_threshold: Duration::from_millis(1500),
        max_flaps_per_minute: 4,
        drain_timeout: Duration::from_secs(15),
        max_concurrent: 750,
    };
    
    // Serialize to YAML
    let yaml = serde_yaml::to_string(&original)?;
    
    // Deserialize back
    let deserialized: ReplayConfig = serde_yaml::from_str(&yaml)?;
    
    assert_eq!(deserialized.lag_threshold, original.lag_threshold);
    assert_eq!(deserialized.recovery_threshold, original.recovery_threshold);
    assert_eq!(deserialized.max_flaps_per_minute, original.max_flaps_per_minute);
    assert_eq!(deserialized.drain_timeout, original.drain_timeout);
    assert_eq!(deserialized.max_concurrent, original.max_concurrent);
    
    Ok(())
}

// =============================================================================
// Performance Log Configuration Tests (v0.8.15+)
// =============================================================================

#[test]
fn test_parse_warmup_period() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 60s
concurrency: 16
warmup_period: 30s

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.warmup_period.is_some());
    assert_eq!(config.warmup_period.unwrap(), std::time::Duration::from_secs(30));
    
    Ok(())
}

#[test]
fn test_parse_warmup_period_minutes() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 5m
warmup_period: 2m

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.warmup_period.is_some());
    assert_eq!(config.warmup_period.unwrap(), std::time::Duration::from_secs(120));
    
    Ok(())
}

#[test]
fn test_parse_warmup_period_none() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 60s

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.warmup_period.is_none());
    
    Ok(())
}

#[test]
fn test_parse_perf_log_config() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 60s

perf_log:
  path: /tmp/perf-metrics.tsv
  interval: 5s

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.perf_log.is_some());
    let perf_log = config.perf_log.as_ref().unwrap();
    assert_eq!(perf_log.path, Some(std::path::PathBuf::from("/tmp/perf-metrics.tsv")));
    assert_eq!(perf_log.interval, std::time::Duration::from_secs(5));
    
    Ok(())
}

#[test]
fn test_parse_perf_log_config_compressed() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 60s

perf_log:
  path: /data/metrics/benchmark.tsv.zst
  interval: 1s

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.perf_log.is_some());
    let perf_log = config.perf_log.as_ref().unwrap();
    assert_eq!(perf_log.path, Some(std::path::PathBuf::from("/data/metrics/benchmark.tsv.zst")));
    assert_eq!(perf_log.interval, std::time::Duration::from_secs(1));
    
    Ok(())
}

#[test]
fn test_parse_perf_log_no_path() -> Result<()> {
    // v0.8.16: path is optional - in distributed mode, perf_log goes to results dir
    let yaml = r#"
target: "file:///tmp/test/"
duration: 60s

perf_log:
  interval: 2s

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.perf_log.is_some());
    let perf_log = config.perf_log.as_ref().unwrap();
    assert_eq!(perf_log.path, None);  // No path specified
    assert_eq!(perf_log.interval, std::time::Duration::from_secs(2));
    
    Ok(())
}

#[test]
fn test_parse_perf_log_default_interval() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 60s

perf_log:
  path: /tmp/perf.tsv

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    assert!(config.perf_log.is_some());
    let perf_log = config.perf_log.as_ref().unwrap();
    // Default interval should be 1 second
    assert_eq!(perf_log.interval, std::time::Duration::from_secs(1));
    
    Ok(())
}

#[test]
fn test_parse_full_perf_config() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 5m
concurrency: 32
warmup_period: 30s

perf_log:
  path: /data/benchmark/perf.tsv.zst
  interval: 1s

workload:
  - op: get
    path: "data/*"
    weight: 80
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 20
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    // Verify warmup
    assert!(config.warmup_period.is_some());
    assert_eq!(config.warmup_period.unwrap(), std::time::Duration::from_secs(30));
    
    // Verify perf_log
    assert!(config.perf_log.is_some());
    let perf_log = config.perf_log.as_ref().unwrap();
    assert_eq!(perf_log.path, Some(std::path::PathBuf::from("/data/benchmark/perf.tsv.zst")));
    assert_eq!(perf_log.interval, std::time::Duration::from_secs(1));
    
    // Verify other fields
    assert_eq!(config.duration, std::time::Duration::from_secs(300));
    assert_eq!(config.concurrency, 32);
    
    Ok(())
}
