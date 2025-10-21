// tests/distributed_config_tests.rs
//! Tests for distributed configuration parsing and validation

use anyhow::Result;
use sai3_bench::config::{Config, DistributedConfig, AgentConfig};

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
    let config_path = "tests/configs/distributed_yaml_test.yaml";
    let yaml = std::fs::read_to_string(config_path)?;
    let config: Config = serde_yaml::from_str(&yaml)?;
    
    assert!(config.distributed.is_some());
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.agents.len(), 2);
    assert_eq!(dist.agents[0].address, "localhost:7761");
    assert_eq!(dist.agents[1].target_override, Some("file:///tmp/sai3bench-test-agent2/".to_string()));
    
    // SSH should be disabled
    assert_eq!(dist.ssh.as_ref().unwrap().enabled, false);
    
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
