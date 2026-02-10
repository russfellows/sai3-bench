// tests/test_stage_config.rs
use sai3_bench::config::{
    CompletionCriteria, DistributedConfig, StageConfig, StageSpecificConfig,
};
use std::time::Duration;

#[test]
fn test_stage_config_execute_parsing() {
    let yaml = r#"
name: epoch-1
order: 1
completion: duration
type: execute
duration: 60s
"#;
    let stage: StageConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(stage.name, "epoch-1");
    assert_eq!(stage.order, 1);
    assert_eq!(stage.completion, CompletionCriteria::Duration);
    
    match stage.config {
        StageSpecificConfig::Execute { duration } => {
            assert_eq!(duration, Duration::from_secs(60));
        }
        _ => panic!("Expected Execute config"),
    }
}

#[test]
fn test_stage_config_prepare_parsing() {
    let yaml = r#"
name: prepare
order: 2
completion: tasks_done
type: prepare
expected_objects: 10000
"#;
    let stage: StageConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(stage.name, "prepare");
    assert_eq!(stage.order, 2);
    assert_eq!(stage.completion, CompletionCriteria::TasksDone);
    
    match stage.config {
        StageSpecificConfig::Prepare { expected_objects } => {
            assert_eq!(expected_objects, Some(10000));
        }
        _ => panic!("Expected Prepare config"),
    }
}

#[test]
fn test_stage_config_cleanup_parsing() {
    let yaml = r#"
name: cleanup
order: 4
completion: tasks_done
optional: true
type: cleanup
expected_objects: 10000
"#;
    let stage: StageConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(stage.name, "cleanup");
    assert_eq!(stage.order, 4);
    assert_eq!(stage.completion, CompletionCriteria::TasksDone);
    assert_eq!(stage.optional, true);
    
    match stage.config {
        StageSpecificConfig::Cleanup { expected_objects } => {
            assert_eq!(expected_objects, Some(10000));
        }
        _ => panic!("Expected Cleanup config"),
    }
}

#[test]
fn test_stage_config_custom_parsing() {
    let yaml = r#"
name: custom-sync
order: 3
completion: script_exit
type: custom
command: /usr/bin/sync_data.sh
args: ["--verbose", "--target", "/mnt/data"]
"#;
    let stage: StageConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(stage.name, "custom-sync");
    assert_eq!(stage.order, 3);
    assert_eq!(stage.completion, CompletionCriteria::ScriptExit);
    
    match stage.config {
        StageSpecificConfig::Custom { command, args } => {
            assert_eq!(command, "/usr/bin/sync_data.sh");
            assert_eq!(args, vec!["--verbose", "--target", "/mnt/data"]);
        }
        _ => panic!("Expected Custom config"),
    }
}

#[test]
fn test_stage_config_hybrid_parsing() {
    let yaml = r#"
name: hybrid-stage
order: 1
completion: duration_or_tasks
type: hybrid
max_duration: 300s
expected_tasks: 5000
"#;
    let stage: StageConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(stage.name, "hybrid-stage");
    assert_eq!(stage.order, 1);
    assert_eq!(stage.completion, CompletionCriteria::DurationOrTasks);
    
    match stage.config {
        StageSpecificConfig::Hybrid { max_duration, expected_tasks } => {
            assert_eq!(max_duration, Some(Duration::from_secs(300)));
            assert_eq!(expected_tasks, Some(5000));
        }
        _ => panic!("Expected Hybrid config"),
    }
}

#[test]
fn test_distributed_config_with_stages() {
    let yaml = r#"
agents:
  - address: "node1:7761"
shared_filesystem: true
tree_creation_mode: coordinator
path_selection: partitioned
stages:
  - name: prepare
    order: 1
    completion: tasks_done
    type: prepare
    expected_objects: 1000
  - name: execute
    order: 2
    completion: duration
    type: execute
    duration: 60s
  - name: cleanup
    order: 3
    completion: tasks_done
    optional: true
    type: cleanup
"#;
    let config: DistributedConfig = serde_yaml::from_str(yaml).unwrap();
    
    assert!(config.stages.is_some());
    let stages = config.stages.as_ref().unwrap();
    assert_eq!(stages.len(), 3);
    
    // Check order field (not YAML position)
    assert_eq!(stages[0].name, "prepare");
    assert_eq!(stages[0].order, 1);
    assert_eq!(stages[1].name, "execute");
    assert_eq!(stages[1].order, 2);
    assert_eq!(stages[2].name, "cleanup");
    assert_eq!(stages[2].order, 3);
}

#[test]
fn test_get_sorted_stages_reorders_by_order_field() {
    // Define stages in scrambled YAML order
    let yaml = r#"
agents:
  - address: "node1:7761"
shared_filesystem: true
tree_creation_mode: coordinator
path_selection: partitioned
stages:
  - name: cleanup
    order: 4
    completion: tasks_done
    type: cleanup
  - name: prepare
    order: 2
    completion: tasks_done
    type: prepare
  - name: execute
    order: 3
    completion: duration
    type: execute
    duration: 60s
  - name: preflight
    order: 1
    completion: duration_or_tasks
    type: hybrid
    max_duration: 300s
"#;
    let config: DistributedConfig = serde_yaml::from_str(yaml).unwrap();
    
    let sorted = config.get_sorted_stages().unwrap();
    assert_eq!(sorted.len(), 4);
    
    // Verify sorted by order field (not YAML position)
    assert_eq!(sorted[0].name, "preflight");
    assert_eq!(sorted[0].order, 1);
    assert_eq!(sorted[1].name, "prepare");
    assert_eq!(sorted[1].order, 2);
    assert_eq!(sorted[2].name, "execute");
    assert_eq!(sorted[2].order, 3);
    assert_eq!(sorted[3].name, "cleanup");
    assert_eq!(sorted[3].order, 4);
}

#[test]
fn test_default_stages_generation() {
    // Config without stages should generate defaults
    let yaml = r#"
agents:
  - address: "node1:7761"
shared_filesystem: true
tree_creation_mode: coordinator
path_selection: partitioned
"#;
    let config: DistributedConfig = serde_yaml::from_str(yaml).unwrap();
    
    assert!(config.stages.is_none());
    
    let stages = config.get_sorted_stages().unwrap();
    assert_eq!(stages.len(), 4);
    
    // Verify default stages
    assert_eq!(stages[0].name, "preflight");
    assert_eq!(stages[0].order, 1);
    assert_eq!(stages[0].completion, CompletionCriteria::ValidationPassed);
    
    assert_eq!(stages[1].name, "prepare");
    assert_eq!(stages[1].order, 2);
    assert_eq!(stages[1].completion, CompletionCriteria::TasksDone);
    
    assert_eq!(stages[2].name, "execute");
    assert_eq!(stages[2].order, 3);
    assert_eq!(stages[2].completion, CompletionCriteria::Duration);
    match &stages[2].config {
        StageSpecificConfig::Execute { duration } => {
            assert_eq!(*duration, Duration::from_secs(120));
        }
        _ => panic!("Expected Execute config"),
    }
    
    assert_eq!(stages[3].name, "cleanup");
    assert_eq!(stages[3].order, 4);
    assert_eq!(stages[3].completion, CompletionCriteria::TasksDone);
    assert_eq!(stages[3].optional, true);
}

#[test]
fn test_validate_duplicate_stage_names() {
    let yaml = r#"
agents:
  - address: "node1:7761"
shared_filesystem: true
tree_creation_mode: coordinator
path_selection: partitioned
stages:
  - name: execute
    order: 1
    completion: duration
    type: execute
    duration: 60s
  - name: execute
    order: 2
    completion: duration
    type: execute
    duration: 60s
"#;
    let config: DistributedConfig = serde_yaml::from_str(yaml).unwrap();
    
    let result = config.get_sorted_stages();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("duplicate stage name"));
}

#[test]
fn test_validate_duplicate_order_values() {
    let yaml = r#"
agents:
  - address: "node1:7761"
shared_filesystem: true
tree_creation_mode: coordinator
path_selection: partitioned
stages:
  - name: prepare
    order: 1
    completion: tasks_done
    type: prepare
  - name: execute
    order: 1
    completion: duration
    type: execute
    duration: 60s
"#;
    let config: DistributedConfig = serde_yaml::from_str(yaml).unwrap();
    
    let result = config.get_sorted_stages();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("duplicate stage order"));
}

#[test]
fn test_validate_completion_criteria_mismatch() {
    // Execute stage with TasksDone completion (should be Duration)
    let yaml = r#"
agents:
  - address: "node1:7761"
shared_filesystem: true
tree_creation_mode: coordinator
path_selection: partitioned
stages:
  - name: execute
    order: 1
    completion: tasks_done
    type: execute
    duration: 60s
"#;
    let config: DistributedConfig = serde_yaml::from_str(yaml).unwrap();
    
    let result = config.get_sorted_stages();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("should use Duration"));
}

#[test]
fn test_stage_with_timeout() {
    let yaml = r#"
name: slow-prepare
order: 1
completion: tasks_done
timeout_secs: 1800
type: prepare
expected_objects: 100000
"#;
    let stage: StageConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(stage.name, "slow-prepare");
    assert_eq!(stage.timeout_secs, Some(1800));
}
