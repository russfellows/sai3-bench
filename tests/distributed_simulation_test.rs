// tests/distributed_simulation_test.rs
//! Simulation tests for distributed controller logic
//! Tests agent selection, config override application, without real SSH/Docker

use anyhow::Result;
use sai3_bench::config::Config;
use std::collections::HashMap;

#[test]
fn test_agent_address_resolution() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 5s

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "vm1.example.com:7761"
      id: "agent-1"
    
    - address: "vm2.example.com"  # No port
      id: "agent-2"
      listen_port: 8888
    
    - address: "10.0.1.50"  # IP without port
      id: "agent-3"
  
  ssh:
    enabled: false  # Direct gRPC mode

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Simulate controller's address resolution logic
    let ssh_enabled = dist.ssh.as_ref().map(|s| s.enabled).unwrap_or(false);
    
    let resolved_addrs: Vec<String> = dist.agents.iter()
        .map(|a| {
            if ssh_enabled {
                // SSH mode: add port if missing
                if a.address.contains(':') {
                    a.address.clone()
                } else {
                    format!("{}:{}", a.address, a.listen_port)
                }
            } else {
                // Direct gRPC mode: use as-is, add default port if needed
                if a.address.contains(':') {
                    a.address.clone()
                } else {
                    format!("{}:{}", a.address, a.listen_port)
                }
            }
        })
        .collect();
    
    assert_eq!(resolved_addrs[0], "vm1.example.com:7761");
    assert_eq!(resolved_addrs[1], "vm2.example.com:8888");
    assert_eq!(resolved_addrs[2], "10.0.1.50:7761");  // Uses default port
    
    println!("✓ Agent addresses resolved correctly:");
    for (idx, addr) in resolved_addrs.iter().enumerate() {
        println!("  Agent {}: {}", idx + 1, addr);
    }
    
    Ok(())
}

#[test]
fn test_per_agent_config_overrides() -> Result<()> {
    let yaml = r#"
target: "s3://default-bucket/data/"
duration: 10s
concurrency: 64

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "agent1:7761"
      id: "reader"
      # Uses defaults: target and concurrency from base config
    
    - address: "agent2:7761"
      id: "writer"
      target_override: "s3://bucket-2/data/"
      concurrency_override: 128
    
    - address: "agent3:7761"
      id: "local"
      target_override: "file:///mnt/nvme/"
      concurrency_override: 32

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Simulate applying overrides
    let base_target = config.target.as_ref().unwrap();
    let base_concurrency = config.concurrency;
    
    for (idx, agent) in dist.agents.iter().enumerate() {
        let effective_target = agent.target_override.as_ref()
            .unwrap_or(base_target);
        let effective_concurrency = agent.concurrency_override
            .unwrap_or(base_concurrency);
        
        println!("Agent {}: target={}, concurrency={}", 
                 agent.id.as_ref().unwrap(), effective_target, effective_concurrency);
        
        match idx {
            0 => {
                assert_eq!(effective_target, "s3://default-bucket/data/");
                assert_eq!(effective_concurrency, 64);
            }
            1 => {
                assert_eq!(effective_target, "s3://bucket-2/data/");
                assert_eq!(effective_concurrency, 128);
            }
            2 => {
                assert_eq!(effective_target, "file:///mnt/nvme/");
                assert_eq!(effective_concurrency, 32);
            }
            _ => panic!("Unexpected agent count"),
        }
    }
    
    println!("✓ Per-agent overrides applied correctly");
    
    Ok(())
}

#[test]
fn test_env_var_injection() -> Result<()> {
    let yaml = r#"
target: "s3://bucket/"
duration: 5s

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "agent1:7761"
      env:
        AWS_PROFILE: "reader"
        RUST_LOG: "info"
    
    - address: "agent2:7761"
      env:
        AWS_PROFILE: "writer"
        RUST_LOG: "debug"
        CUSTOM_VAR: "custom-value"

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Simulate env var collection for Docker commands
    for agent in &dist.agents {
        println!("Agent: {}", agent.address);
        for (key, value) in &agent.env {
            println!("  -e {}={}", key, value);
        }
    }
    
    assert_eq!(dist.agents[0].env.len(), 2);
    assert_eq!(dist.agents[1].env.len(), 3);
    assert_eq!(dist.agents[1].env.get("CUSTOM_VAR"), Some(&"custom-value".to_string()));
    
    println!("✓ Environment variables configured correctly");
    
    Ok(())
}

#[test]
fn test_volume_mount_specifications() -> Result<()> {
    let yaml = r#"
target: "file:///data/"
duration: 5s

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "agent1:7761"
      volumes:
        - "/mnt/nvme:/data"
        - "/tmp/results:/results:ro"
    
    - address: "agent2:7761"
      volumes:
        - "/mnt/ssd:/mnt/ssd"

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Simulate volume mount command generation
    for agent in &dist.agents {
        println!("Agent: {}", agent.address);
        for volume in &agent.volumes {
            println!("  -v {}", volume);
        }
    }
    
    assert_eq!(dist.agents[0].volumes.len(), 2);
    assert_eq!(dist.agents[0].volumes[1], "/tmp/results:/results:ro");
    assert_eq!(dist.agents[1].volumes[0], "/mnt/ssd:/mnt/ssd");
    
    println!("✓ Volume mounts parsed correctly");
    
    Ok(())
}

#[test]
fn test_path_template_resolution() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 5s

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "agent1:7761"
    - address: "agent2:7761"
    - address: "agent3:7761"
  
  path_template: "agent-{id}/"

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    // Simulate path template resolution (like controller does)
    let path_template = &dist.path_template;
    
    for idx in 0..dist.agents.len() {
        let resolved_path = path_template.replace("{id}", &(idx + 1).to_string());
        println!("Agent {}: path prefix = {}", idx + 1, resolved_path);
        
        assert_eq!(resolved_path, format!("agent-{}/", idx + 1));
    }
    
    println!("✓ Path templates resolved correctly");
    
    Ok(())
}

#[test]
fn test_backward_compatibility_cli_agents() -> Result<()> {
    // Config without distributed section
    let yaml = r#"
target: "file:///tmp/test/"
duration: 5s
concurrency: 32

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    
    // Simulate controller logic
    let cli_agents = vec!["agent1:7761".to_string(), "agent2:7761".to_string()];
    
    let (agent_addrs, ssh_deployment) = if let Some(ref dist_config) = config.distributed {
        // Would use config.distributed.agents
        let addrs: Vec<String> = dist_config.agents.iter()
            .map(|a| a.address.clone())
            .collect();
        (addrs, Some(dist_config.clone()))
    } else {
        // Fallback to CLI agents
        (cli_agents.clone(), None)
    };
    
    assert_eq!(agent_addrs, cli_agents);
    assert!(ssh_deployment.is_none());
    
    println!("✓ Backward compatibility maintained - CLI --agents works");
    
    Ok(())
}

#[test]
fn test_mixed_storage_backend_agents() -> Result<()> {
    let yaml = r#"
target: "s3://default-bucket/"
duration: 5s

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "cloud-agent-1:7761"
      id: "s3-1"
      # Uses default S3 target
    
    - address: "cloud-agent-2:7761"
      id: "s3-2"
      target_override: "s3://bucket-2/"
    
    - address: "local-agent:7761"
      id: "local"
      target_override: "file:///mnt/local/"
      volumes:
        - "/mnt/local:/mnt/local"
    
    - address: "azure-agent:7761"
      id: "azure"
      target_override: "az://storage-account/container/"
      env:
        AZURE_STORAGE_ACCOUNT: "testaccount"

workload:
  - op: get
    path: "data/*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    
    assert_eq!(dist.agents.len(), 4);
    
    // Verify we can mix different storage backends
    let backends: Vec<String> = dist.agents.iter()
        .map(|a| {
            let target = a.target_override.as_ref()
                .unwrap_or(config.target.as_ref().unwrap());
            if target.starts_with("s3://") {
                "S3".to_string()
            } else if target.starts_with("az://") {
                "Azure".to_string()
            } else if target.starts_with("file://") {
                "File".to_string()
            } else {
                "Unknown".to_string()
            }
        })
        .collect();
    
    assert_eq!(backends[0], "S3");
    assert_eq!(backends[1], "S3");
    assert_eq!(backends[2], "File");
    assert_eq!(backends[3], "Azure");
    
    println!("✓ Mixed storage backends supported:");
    for (idx, backend) in backends.iter().enumerate() {
        println!("  Agent {}: {}", idx + 1, backend);
    }
    
    Ok(())
}

#[test]
fn test_deployment_config_docker_command_building() -> Result<()> {
    let yaml = r#"
target: "file:///tmp/test/"
duration: 5s

distributed:
  shared_filesystem: false
  tree_creation_mode: isolated
  path_selection: random
  agents:
    - address: "vm1.example.com"
      id: "agent-1"
      listen_port: 7761
      env:
        RUST_LOG: "info"
        AWS_PROFILE: "test"
      volumes:
        - "/data:/data"
  
  deployment:
    deploy_type: "docker"
    image: "sai3bench:v0.6.11"
    network_mode: "host"
    docker_args:
      - "--cpus=2"

workload:
  - op: get
    path: "*"
    weight: 100
"#;

    let config: Config = serde_yaml::from_str(yaml)?;
    let dist = config.distributed.as_ref().unwrap();
    let deploy = dist.deployment.as_ref().unwrap();
    let agent = &dist.agents[0];
    
    // Simulate building docker run command (like ssh_deploy.rs does)
    let mut docker_cmd = vec![
        "docker run -d".to_string(),
        "--rm".to_string(),
        format!("--name sai3bench-agent-{}", agent.id.as_ref().unwrap()),
        format!("--network {}", deploy.network_mode),
    ];
    
    // Add env vars
    for (key, value) in &agent.env {
        docker_cmd.push(format!("-e {}={}", key, value));
    }
    
    // Add volumes
    for volume in &agent.volumes {
        docker_cmd.push(format!("-v {}", volume));
    }
    
    // Add extra args
    for arg in &deploy.docker_args {
        docker_cmd.push(arg.clone());
    }
    
    // Image and command
    docker_cmd.push(deploy.image.clone());
    docker_cmd.push("sai3bench-agent".to_string());
    docker_cmd.push("--listen".to_string());
    docker_cmd.push(format!("0.0.0.0:{}", agent.listen_port));
    
    let full_cmd = docker_cmd.join(" ");
    
    println!("Simulated docker command:");
    println!("{}", full_cmd);
    
    assert!(full_cmd.contains("--network host"));
    assert!(full_cmd.contains("-e RUST_LOG=info"));
    assert!(full_cmd.contains("-v /data:/data"));
    assert!(full_cmd.contains("--cpus=2"));
    assert!(full_cmd.contains("sai3bench:v0.6.11"));
    assert!(full_cmd.contains("--listen 0.0.0.0:7761"));
    
    println!("✓ Docker command built correctly");
    
    Ok(())
}
