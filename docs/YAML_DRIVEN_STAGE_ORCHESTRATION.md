# YAML-Driven Stage Orchestration Design

**Created**: February 4, 2026  
**Status**: Design Document (to be implemented)  
**Supersedes**: Hardcoded state transitions in v0.8.25

---

## Problem with Current Design

**v0.8.25 has HARDCODED stage transitions:**
```rust
Idle → Validating → PrepareReady → Preparing → ExecuteReady → Executing → CleanupReady → Cleaning → Completed
```

**Critical Issues:**
1. ❌ Assumes cleanup always happens after execution
2. ❌ Cannot skip stages (what if no cleanup needed?)
3. ❌ Cannot have multiple execution stages (multi-epoch training, checkpoint testing)
4. ❌ Stage order decided at compile time, not runtime
5. ❌ Agents and controller must agree on stage order via code, not config

**Example Failure Cases:**
- Benchmark without cleanup → stuck at CleanupReady waiting forever
- Multi-stage training → no way to express "execute epoch 1, then epoch 2, then epoch 3"
- Checkpoint workflow → can't do "load → execute → save" sequence

---

## Solution: Explicit Stage Ordering in YAML

### Core Principles

1. **YAML defines what stages exist and their execution order**
2. **Explicit `order` field** - no reliance on YAML positional ordering
3. **Controller and agents read same YAML** - guaranteed agreement on stage sequence
4. **Flexible stage types** - validation, prepare, execute, cleanup, custom
5. **Optional stages** - can skip if previous stage fails or based on config

---

## YAML Schema

### Basic Example (Simple Benchmark)

```yaml
distributed:
  agents:
    - address: "host1:7760"
      id: "agent-1"
    - address: "host2:7760"
      id: "agent-2"
  
  # Explicit stage orchestration
  stages:
    - name: "validation"
      order: 1                    # Explicit ordering
      type: preflight             # Stage type (predefined behavior)
      barrier: all_or_nothing     # Barrier synchronization type
      timeout_secs: 30            # How long to wait at barrier
      
    - name: "prepare"
      order: 2
      type: prepare
      barrier: majority
      timeout_secs: 600
      
    - name: "execute_workload"
      order: 3
      type: execute
      barrier: all_or_nothing
      timeout_secs: 3600
      
    - name: "cleanup"
      order: 4
      type: cleanup
      barrier: best_effort
      timeout_secs: 300
      optional: true              # Skip if earlier stage fails
```

### Advanced Example (Multi-Epoch Training)

```yaml
distributed:
  stages:
    - name: "validation"
      order: 1
      type: preflight
      barrier: all_or_nothing
      completion: validation_passed  # Completes when validation succeeds
      
    - name: "prepare_dataset"
      order: 2
      type: prepare
      barrier: majority
      completion: tasks_done         # Completes when all objects created (NOT time-based!)
      
    - name: "load_checkpoint"
      order: 3
      type: custom
      barrier: all_or_nothing
      completion: script_exit        # Completes when script exits successfully
      script: "load_checkpoint.sh"
      
    - name: "epoch_1"
      order: 4
      type: execute
      barrier: all_or_nothing
      completion: duration           # TIME-BASED: runs for fixed duration
      duration_secs: 300
      description: "Training epoch 1"
      
    - name: "save_checkpoint_1"
      order: 5
      type: custom
      barrier: majority
      completion: script_exit
      script: "save_checkpoint.sh"
      
    - name: "epoch_2"
      order: 6
      type: execute
      barrier: all_or_nothing
      completion: duration           # TIME-BASED: runs for fixed duration
      duration_secs: 300
      description: "Training epoch 2"
      
    - name: "save_checkpoint_2"
      order: 7
      type: custom
      barrier: majority
      completion: script_exit
      script: "save_checkpoint.sh"
      
    - name: "cleanup"
      order: 8
      type: cleanup
      barrier: best_effort
      completion: tasks_done         # Completes when all objects deleted (NOT time-based!)
      optional: true
```

### Example with Stage Reordering in YAML (Still Works!)

```yaml
distributed:
  stages:
    # Define stages in any YAML order - explicit 'order' field controls execution
    
    - name: "cleanup"
      order: 4              # Executes FOURTH (despite being first in YAML)
      type: cleanup
      barrier: best_effort
      
    - name: "validation"
      order: 1              # Executes FIRST
      type: preflight
      barrier: all_or_nothing
      
    - name: "execute_workload"
      order: 3              # Executes THIRD
      type: execute
      barrier: all_or_nothing
      
    - name: "prepare"
      order: 2              # Executes SECOND
      type: prepare
      barrier: majority
```

**Controller sorts by `order` field → execution order is always correct!**

---

## Stage Types

### Completion Criteria (How Stage Knows It's Done)

**CRITICAL**: Different stages have different ways of knowing when they're complete!

#### 1. `duration` - Time-Based Completion
- **Used By**: Execute stages (I/O workload benchmarks)
- **Behavior**: Run for exactly N seconds, then complete
- **Example**: `completion: duration`, `duration_secs: 300`
- **Progress**: Report elapsed time / total time

#### 2. `tasks_done` - Task-Based Completion  
- **Used By**: Prepare, Cleanup stages
- **Behavior**: Complete when all tasks finished (objects created/deleted, etc.)
- **Example**: `completion: tasks_done`
- **Progress**: Report objects_processed / objects_total
- **CANNOT assign time** - takes however long it takes!

#### 3. `script_exit` - Script-Based Completion
- **Used By**: Custom stages
- **Behavior**: Complete when script/command exits successfully (exit code 0)
- **Example**: `completion: script_exit`, `script: "load_checkpoint.sh"`
- **Progress**: Report script running/completed

#### 4. `validation_passed` - Validation-Based Completion
- **Used By**: Preflight stages
- **Behavior**: Complete when all validation checks pass
- **Example**: `completion: validation_passed`
- **Progress**: Report checks_passed / checks_total

#### 5. `duration_or_tasks` - Hybrid Completion (Whichever First)
- **Used By**: Any stage
- **Behavior**: Complete when EITHER duration reached OR all tasks done
- **Example**: `completion: duration_or_tasks`, `duration_secs: 600`, `max_objects: 10000`
- **Use Case**: "Create 10k objects OR 10 minutes, whichever comes first"
- **Progress**: Report both time and task completion

---

### Stage Type Details

### 1. `preflight` - Pre-flight Validation
- **Behavior**: Runs validation checks (storage access, permissions, etc.)
- **Completion**: `validation_passed` (when all checks pass)
- **Typical Duration**: 10-60 seconds
- **Default Barrier**: `all_or_nothing` (any failure aborts workload)

### 2. `prepare` - Object/Data Preparation
- **Behavior**: Creates objects, generates data, sets up test environment
- **Completion**: `tasks_done` (when all objects created) - NOT time-based!
- **Typical Duration**: Varies widely (seconds to hours based on object count/size)
- **Default Barrier**: `majority` (can proceed with reduced agent count)
- **Progress Tracking**: Objects created / total objects

### 3. `execute` - Workload Execution
- **Behavior**: Runs I/O workload (GET/PUT/LIST operations)
- **Completion**: `duration` (runs for fixed time) - TIME-based!
- **Typical Duration**: Seconds to hours (config-dependent)
- **Default Barrier**: `all_or_nothing` (need all agents for accurate results)
- **Can Have Multiple**: Yes! (epoch_1, epoch_2, epoch_3, etc.)
- **Progress Tracking**: Elapsed time / duration

### 4. `cleanup` - Post-Execution Cleanup
- **Behavior**: Deletes objects, removes temporary files
- **Completion**: `tasks_done` (when all objects deleted) - NOT time-based!
- **Typical Duration**: Varies widely (seconds to hours based on object count)
- **Default Barrier**: `best_effort` (proceed with survivors)
- **Often Optional**: Yes (may skip if earlier stages fail)
- **Progress Tracking**: Objects deleted / total objects

### 5. `custom` - User-Defined Stage
- **Behavior**: Runs custom script/command on each agent
- **Completion**: `script_exit` (when script exits successfully)
- **Examples**: Load checkpoint, save checkpoint, process results, archive logs
- **Barrier**: User-configured
- **Execution**: Via `script` field or `command` field
- **Progress Tracking**: Script running/completed

---

## State Machine Redesign

### New WorkloadState Enum

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkloadState {
    Idle,                           // Ready to accept new workload
    
    // Stage execution states
    AtStage {
        stage_index: usize,         // Which stage (0-based index into sorted stages)
        stage_name: String,         // Human-readable stage name
        ready_for_next: bool,       // true = at barrier, waiting for controller
    },
    
    // Terminal states
    Completed,                      // All stages done successfully
    Failed(String),                 // Error occurred (with reason)
    Aborting,                       // Emergency shutdown in progress
}
```

### State Transitions (YAML-Driven)

```rust
// Controller sends stage sequence to agents at start
// Both agree on stage ordering from YAML

// Stage-by-stage execution:
Idle 
  → AtStage { index: 0, name: "validation", ready: false }      // Start stage 0
  → AtStage { index: 0, name: "validation", ready: true }       // Stage 0 done, at barrier
  → AtStage { index: 1, name: "prepare", ready: false }         // Barrier released, start stage 1
  → AtStage { index: 1, name: "prepare", ready: true }          // Stage 1 done, at barrier
  → AtStage { index: 2, name: "execute_workload", ready: false }
  → AtStage { index: 2, name: "execute_workload", ready: true }
  → AtStage { index: 3, name: "cleanup", ready: false }
  → AtStage { index: 3, name: "cleanup", ready: true }
  → Completed                                                    // All stages done
  → Idle                                                         // Ready for next workload
```

**Key Advantages:**
- ✅ No hardcoded stage names in state machine
- ✅ Supports any number of stages (1 to N)
- ✅ Supports stage reordering via YAML
- ✅ Supports optional stages (skip if needed)
- ✅ Supports multiple execute stages (epochs, rounds, etc.)

---

## Configuration Struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageConfig {
    /// Stage name (must be unique)
    pub name: String,
    
    /// Explicit execution order (1-based for human readability)
    /// Controller sorts stages by this field before execution
    pub order: u32,
    
    /// Stage type determines behavior
    #[serde(rename = "type")]
    pub stage_type: StageType,
    
    /// Barrier synchronization type
    pub barrier: BarrierType,
    
    /// Timeout for barrier wait (seconds)
    #[serde(default = "default_stage_timeout")]
    pub timeout_secs: u64,
    
    /// Optional stage (skip if previous stage fails)
    #[serde(default)]
    pub optional: bool,
    
    /// How this stage knows it's complete (CRITICAL!)
    pub completion: CompletionCriteria,
    
    /// Stage-specific configuration
    #[serde(flatten)]
    pub config: StageSpecificConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionCriteria {
    /// Time-based: runs for fixed duration (execute stages)
    Duration,
    
    /// Task-based: completes when all tasks done (prepare/cleanup)
    TasksDone,
    
    /// Script-based: completes when script exits successfully (custom stages)
    ScriptExit,
    
    /// Validation-based: completes when all checks pass (preflight)
    ValidationPassed,
    
    /// Hybrid: whichever comes first (duration OR tasks)
    DurationOrTasks,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StageType {
    Preflight,
    Prepare,
    Execute,
    Cleanup,
    Custom { script: String },  // User-defined stage
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StageSpecificConfig {
    Execute {
        /// For duration-based completion
        duration_secs: Option<u64>,
        description: Option<String>,
    },
    Prepare {
        /// For task-based completion (prepare stage)
        /// Total objects to create (progress tracking)
        expected_objects: Option<u64>,
        /// Max duration as safety timeout (optional)
        max_duration_secs: Option<u64>,
    },
    Cleanup {
        /// For task-based completion (cleanup stage)
        /// Total objects to delete (progress tracking)
        expected_objects: Option<u64>,
        /// Max duration as safety timeout (optional)
        max_duration_secs: Option<u64>,
    },
    Custom {
        command: Option<String>,
        env: Option<HashMap<String, String>>,
        /// For script-based completion
        timeout_secs: Option<u64>,  // Kill script if exceeds timeout
    },
    Hybrid {
        /// For duration_or_tasks completion
        duration_secs: u64,          // Complete after this duration
        max_objects: u64,            // OR when this many objects processed
    },
    Other,  // Preflight uses validation-based completion
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedConfig {
    pub agents: Vec<AgentConfig>,
    
    /// Explicit stage ordering (replaces hardcoded transitions)
    pub stages: Vec<StageConfig>,
    
    // ... existing fields ...
}
```

---

## Controller Implementation

### Stage Orchestration Flow

```rust
// 1. Parse YAML and sort stages by order field
let mut stages = config.distributed.stages.clone();
stages.sort_by_key(|s| s.order);

// 2. Validate stage ordering (no gaps, no duplicates)
validate_stage_order(&stages)?;

// 3. Send stage sequence to all agents
for agent in &agents {
    agent.send_stage_sequence(&stages).await?;
}

// 4. Execute stages in order
for (index, stage) in stages.iter().enumerate() {
    info!("Starting stage {}: {}", index, stage.name);
    
    // Initialize barrier for this stage
    let barrier = BarrierManager::new(
        agents.clone(),
        stage.barrier,
        stage.timeout_secs,
    );
    
    // Signal agents to start stage
    for agent in &agents {
        agent.start_stage(index).await?;
    }
    
    // Wait for barrier (all agents reach stage completion)
    let barrier_result = barrier.wait_for_ready().await?;
    
    match barrier_result {
        BarrierStatus::Ready => {
            info!("Stage {} complete - all agents ready", stage.name);
        }
        BarrierStatus::Degraded => {
            warn!("Stage {} complete - some agents failed", stage.name);
            if stage.optional {
                info!("Stage is optional, continuing");
            } else {
                return Err(anyhow!("Required stage failed"));
            }
        }
        BarrierStatus::Failed => {
            error!("Stage {} failed", stage.name);
            if stage.optional {
                info!("Stage is optional, skipping");
                continue;
            } else {
                return Err(anyhow!("Required stage failed"));
            }
        }
    }
}

info!("All stages completed successfully");
```

---

## Agent Implementation

### Stage Execution Loop

```rust
// Agent receives stage sequence from controller at startup
let stages: Vec<StageConfig> = receive_stage_sequence().await?;

// Execute stages in order
for (index, stage) in stages.iter().enumerate() {
    // Transition to stage execution state
    transition_to(WorkloadState::AtStage {
        stage_index: index,
        stage_name: stage.name.clone(),
        ready_for_next: false,
    }).await?;
    
    // Execute stage based on type
    match stage.stage_type {
        StageType::Preflight => {
            run_preflight_validation(&config).await?;
        }
        StageType::Prepare => {
            run_prepare_phase(&config).await?;
        }
        StageType::Execute => {
            run_execute_phase(&config, stage.config).await?;
        }
        StageType::Cleanup => {
            run_cleanup_phase(&config).await?;
        }
        StageType::Custom { script } => {
            run_custom_stage(&script, &stage.config).await?;
        }
    }
    
    // Mark stage complete, report barrier readiness
    transition_to(WorkloadState::AtStage {
        stage_index: index,
        stage_name: stage.name.clone(),
        ready_for_next: true,
    }).await?;
    
    // Report barrier readiness to controller
    report_barrier_ready(index, &stage.name).await?;
    
    // Wait for controller to release barrier (proceed to next stage)
    wait_for_barrier_release().await?;
}

// All stages done
transition_to(WorkloadState::Completed).await?;
```

---

## Migration Path

### Phase 1: Add Stage Configuration (Backward Compatible)

1. Add `stages` field to DistributedConfig (optional for now)
2. If `stages` not specified, generate default stage sequence:
   ```rust
   vec![
       StageConfig { name: "validation", order: 1, type: Preflight, ... },
       StageConfig { name: "prepare", order: 2, type: Prepare, ... },
       StageConfig { name: "execute", order: 3, type: Execute, ... },
       StageConfig { name: "cleanup", order: 4, type: Cleanup, optional: true, ... },
   ]
   ```
3. Existing configs work without changes (use default stages)

### Phase 2: Update State Machine

1. Replace hardcoded state transitions with `AtStage` state
2. Controller sorts stages by `order` field
3. Agents execute stages in sorted order
4. Both controller and agents use same sorted stage list

### Phase 3: Test with Multi-Stage Workflows

1. Create test configs with custom stage ordering
2. Test multi-epoch execution (stage reordering)
3. Test optional stages (cleanup failures)
4. Test custom stages (user scripts)

### Phase 4: Deprecate Hardcoded Transitions

1. Remove old WorkloadState variants (Preparing, Executing, Cleaning, etc.)
2. All workflows use YAML-driven stage orchestration
3. Update documentation with new stage configuration

---

## Benefits

### For Users
- ✅ **Flexible workflows** - define any stage sequence
- ✅ **Clear intent** - explicit `order` field shows execution sequence
- ✅ **Multi-stage support** - epochs, rounds, checkpoints, etc.
- ✅ **Optional stages** - cleanup, checkpointing, etc.
- ✅ **Custom stages** - run user scripts at any point

### For Developers
- ✅ **No hardcoded assumptions** - stage order in YAML, not code
- ✅ **Simpler state machine** - one `AtStage` state, not 8 specific states
- ✅ **Easier testing** - mock stage sequences via config
- ✅ **Better maintainability** - add new stage types without code changes

### For Distributed Coordination
- ✅ **Guaranteed agreement** - controller and agents read same YAML
- ✅ **Explicit synchronization** - barrier at each stage boundary
- ✅ **Clear progress tracking** - always know which stage executing
- ✅ **Failure isolation** - know exactly which stage failed

---

## Example Configs

### Simple Benchmark (No Cleanup)
```yaml
distributed:
  stages:
    - name: "validation"
      order: 1
      type: preflight
      barrier: all_or_nothing
      completion: validation_passed
      
    - name: "prepare"
      order: 2
      type: prepare
      barrier: majority
      completion: tasks_done        # NOT time-based! Completes when all objects created
      expected_objects: 10000       # For progress tracking
      
    - name: "execute"
      order: 3
      type: execute
      barrier: all_or_nothing
      completion: duration          # TIME-based! Runs for fixed duration
      duration_secs: 300
```

### Multi-Epoch Training
```yaml
distributed:
  stages:
    - name: "validation"
      order: 1
      type: preflight
      barrier: all_or_nothing
      completion: validation_passed
      
    - name: "prepare"
      order: 2
      type: prepare
      barrier: majority
      completion: tasks_done        # Task-based completion
      expected_objects: 100000
      
    - name: "epoch_1"
      order: 3
      type: execute
      barrier: all_or_nothing
      completion: duration          # Time-based completion
      duration_secs: 300
      
    - name: "epoch_2"
      order: 4
      type: execute
      barrier: all_or_nothing
      completion: duration          # Time-based completion
      duration_secs: 300
      
    - name: "epoch_3"
      order: 5
      type: execute
      barrier: all_or_nothing
      completion: duration          # Time-based completion
      duration_secs: 300
      
    - name: "cleanup"
      order: 6
      type: cleanup
      barrier: best_effort
      completion: tasks_done        # Task-based completion
      expected_objects: 100000
      optional: true
```

### Checkpoint Workflow
```yaml
distributed:
  stages:
    - name: "validation"
      order: 1
      type: preflight
      barrier: all_or_nothing
      completion: validation_passed
      
    - name: "load_checkpoint"
      order: 2
      type: custom
      script: "load_ckpt.sh"
      barrier: all_or_nothing
      completion: script_exit       # Script-based completion
      timeout_secs: 300
      
    - name: "execute"
      order: 3
      type: execute
      barrier: all_or_nothing
      completion: duration          # Time-based completion
      duration_secs: 600
      
    - name: "save_checkpoint"
      order: 4
      type: custom
      script: "save_ckpt.sh"
      barrier: majority
      completion: script_exit       # Script-based completion
      timeout_secs: 300
```

### Hybrid Example (Prepare with Timeout)
```yaml
distributed:
  stages:
    - name: "prepare_with_timeout"
      order: 2
      type: prepare
      barrier: majority
      completion: duration_or_tasks  # Whichever comes first!
      duration_secs: 600            # Max 10 minutes
      max_objects: 10000            # OR 10k objects, whichever first
      description: "Create objects but don't wait forever"
```

---

## Next Steps

1. **Implement StageConfig parsing** - add `stages` field to DistributedConfig
2. **Update state machine** - replace hardcoded states with `AtStage`
3. **Implement stage sorting** - controller sorts by `order` field
4. **Wire up agent execution** - agents execute stages in sorted order
5. **Add RPC for stage sequence** - controller sends sorted stages to agents
6. **Test multi-stage workflows** - verify epoch-based execution works
7. **Document YAML schema** - update README with stage configuration examples

---

**Status**: Design complete, ready for implementation  
**Next**: Implement Phase 1 (add `stages` field with backward compatibility)
