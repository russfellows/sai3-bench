# Directory Tree for Shared Filesystems - Design

## Problem Statement

When running distributed tests with multiple workers/agents against a **shared filesystem** (NFS, Lustre, parallel FS, or even S3/GCS/Azure), we need to handle directory tree creation carefully to avoid:

1. **Race conditions**: Multiple agents trying to create the same directory simultaneously
2. **Conflicts**: Workers expecting independent directory spaces but sharing the same tree
3. **Data corruption**: Concurrent metadata operations on the same paths

## Current State

### Existing Infrastructure
- **`apply_agent_prefix()`**: Can add per-agent path prefixes (e.g., `agent-1/`, `agent-2/`)
- **`shared_storage` parameter**: Distinguishes shared backends (S3/GCS/Azure) from local (file://)
- **Idempotent mkdir**: `rmdir()` already treats `NotFound` as success
- **Prepare phase**: `--prepare-only` and `--skip-prepare` flags for pre-population

### What's Missing
- **Directory tree configuration** not integrated into `PrepareConfig` yet
- **Shared vs isolated semantics** not defined for directory trees
- **Coordinator role** for "create once, use many" pattern

## Design Options

### Option 1: Per-Agent Isolation (Independent Trees)
**Model**: Each agent creates its own isolated directory tree under its prefix

```yaml
distributed:
  agents:
    - address: "node1:7761"
      id: "agent-1"
    - address: "node2:7761"
      id: "agent-2"
  path_template: "agent-{id}/"  # Each agent isolated

target: "file:///mnt/shared_nfs/bench/"

prepare:
  directory_structure:
    width: 4
    depth: 3
    files_per_dir: 100
    distribution: "bottom"
```

**Result**: 
- Agent 1 creates: `/mnt/shared_nfs/bench/agent-1/sai3bench.d1_w1.dir/...`
- Agent 2 creates: `/mnt/shared_nfs/bench/agent-2/sai3bench.d1_w1.dir/...`

**Pros**:
- No coordination needed
- No race conditions
- Simple to implement (reuse existing `apply_agent_prefix`)
- Each agent can have independent tree sizes/configs

**Cons**:
- Doesn't test "shared metadata workload" scenarios
- More storage space needed (duplicate trees)
- Not realistic for multi-client shared FS benchmarking

### Option 2: Shared Tree with Coordinator (Create Once)
**Model**: Controller/coordinator creates tree once, all agents use it

```yaml
distributed:
  shared_filesystem: true  # NEW FLAG
  coordinator_creates_tree: true  # Controller creates, agents skip
  
target: "file:///mnt/shared_nfs/bench/"

prepare:
  directory_structure:
    width: 4
    depth: 3
    files_per_dir: 100
    distribution: "bottom"
```

**Flow**:
1. Controller creates full directory tree before launching agents
2. Agents start with `--skip-prepare` (tree already exists)
3. Agents perform workload operations on shared tree

**Pros**:
- Realistic shared FS testing (multiple clients, one namespace)
- Avoids race conditions during creation
- Clean separation: prepare once, test many

**Cons**:
- Requires controller orchestration
- Single-threaded tree creation (slower for huge trees)
- Doesn't test concurrent mkdir performance

### Option 3: Race-Safe Shared Tree (Idempotent Creates)
**Model**: All agents try to create full tree, idempotent mkdir handles races

```yaml
distributed:
  shared_filesystem: true
  concurrent_tree_creation: true  # All agents create, mkdir is idempotent
  
target: "file:///mnt/shared_nfs/bench/"

prepare:
  directory_structure:
    width: 4
    depth: 3
```

**Flow**:
1. All agents start prepare phase simultaneously
2. Each agent tries to create all directories
3. Idempotent mkdir (already implemented) handles conflicts:
   - First agent succeeds
   - Other agents get "already exists" but treat as success

**Pros**:
- Tests concurrent metadata performance (realistic for parallel FS)
- Fastest tree creation (parallel from N agents)
- Simple config (no special flags needed)

**Cons**:
- Wasted work (agents duplicate effort)
- Slightly higher metadata load during creation
- Requires truly idempotent operations

## Recommended Approach (APPROVED)

**Option 3: Race-Safe Concurrent Creation** with explicit configuration and tunable contention control

### Configuration Schema
```yaml
distributed:
  shared_filesystem: bool     # REQUIRED: Must be explicitly true or false
                              # NO auto-detection - user must specify intent
  
  tree_creation_mode: string  # REQUIRED: "isolated" | "coordinator" | "concurrent"
                              # NO default - forces explicit choice
  
  path_selection: string      # Controls runtime contention level
                              # Default: "random" (maximum contention)
                              # Options: "random" | "partitioned" | "exclusive" | "weighted"
  
  partition_overlap: float    # For "partitioned"/"weighted" modes (0.0-1.0)
                              # Probability of accessing other agents' partitions
                              # Default: 0.3 (30% overlap)
```

### Tree Creation Modes

| Mode | Behavior | When to Use |
|------|----------|-------------|
| `isolated` | Each agent creates own tree under `agent-{id}/` | Local disks, independent testing |
| `coordinator` | Controller creates once, agents skip prepare | Low contention setup, huge trees |
| `concurrent` | All agents race-safely create (idempotent mkdir) | **Recommended for shared FS** - tests real contention |

### Path Selection Strategies (Runtime Contention Control)

| Strategy | Contention Level | Behavior |
|----------|------------------|----------|
| `random` | **Maximum** | All agents pick any directory - realistic shared workload |
| `partitioned` | **Reduced** | Agents prefer `hash(path) % agent_id` dirs - some overlap |
| `exclusive` | **Minimal** | Each agent only uses assigned directories - baseline |
| `weighted` | **Tunable** | Probabilistic mix controlled by `partition_overlap` |

### Implementation Plan

1. **Add config fields** to `DistributedConfig`:
   ```rust
   pub struct DistributedConfig {
       // ...existing fields...
       
       /// Whether target filesystem is shared across agents (REQUIRED - no default)
       /// Must be explicitly set to true or false
       pub shared_filesystem: bool,
       
       /// How to handle directory tree creation (REQUIRED - no default)
       /// - "isolated": Each agent creates separate tree under agent-{id}/
       /// - "coordinator": Controller creates tree, agents skip prepare
       /// - "concurrent": All agents create tree (idempotent operations)
       pub tree_creation_mode: TreeCreationMode,
       
       /// How agents select paths during workload (affects contention level)
       /// Default: "random" for maximum contention (realistic)
       #[serde(default = "default_path_selection")]
       pub path_selection: PathSelectionStrategy,
       
       /// For "partitioned"/"weighted" modes: overlap probability (0.0-1.0)
       /// Default: 0.3 (30% chance to access another agent's partition)
       #[serde(default = "default_partition_overlap")]
       pub partition_overlap: f64,
   }
   
   #[derive(Debug, Deserialize, Clone)]
   #[serde(rename_all = "lowercase")]
   pub enum TreeCreationMode {
       Isolated,
       Coordinator,
       Concurrent,
   }
   
   #[derive(Debug, Deserialize, Clone)]
   #[serde(rename_all = "lowercase")]
   pub enum PathSelectionStrategy {
       Random,        // Any path, max contention
       Partitioned,   // Prefer hash(path) % agent_id
       Exclusive,     // Only assigned partition
       Weighted,      // Probabilistic mix
   }
   
   fn default_path_selection() -> PathSelectionStrategy {
       PathSelectionStrategy::Random  // Maximum contention by default
   }
   
   fn default_partition_overlap() -> f64 {
       0.3  // 30% overlap
   }
   ```

2. **Add directory_structure to PrepareConfig**:
   ```rust
   pub struct PrepareConfig {
       // ...existing fields...
       
       /// Optional directory tree structure (rdf-bench style width/depth)
       #[serde(default)]
       pub directory_structure: Option<DirectoryStructureConfig>,
   }
   ```

3. **Implement tree creation logic** in prepare phase:
   ```rust
   if let Some(dir_config) = &prepare.directory_structure {
       let tree = DirectoryTree::new(dir_config.clone())?;
       
       match tree_creation_mode {
           TreeCreationMode::Isolated => {
               // Create under agent-specific prefix (already handled by apply_agent_prefix)
               create_tree(&tree, store)?;
           }
           TreeCreationMode::Coordinator => {
               if is_coordinator {
                   create_tree(&tree, store)?;
               }
               // Agents skip (use --skip-prepare)
           }
           TreeCreationMode::Concurrent => {
               // All agents create (idempotent mkdir handles races)
               create_tree(&tree, store)?;
           }
       }
   }
   ```

4. **Implement path selection logic** in workload:
   ```rust
   fn select_directory_path(
       tree: &DirectoryTree, 
       agent_id: usize, 
       num_agents: usize,
       strategy: &PathSelectionStrategy,
       overlap: f64,
   ) -> &str {
       match strategy {
           PathSelectionStrategy::Random => {
               // Pick any directory (maximum contention)
               tree.all_paths().choose(&mut rand::thread_rng()).unwrap()
           }
           PathSelectionStrategy::Partitioned => {
               // Prefer own partition, but allow overlap
               if rand::random::<f64>() < overlap {
                   // Access another partition (contention)
                   tree.all_paths().choose(&mut rand::thread_rng()).unwrap()
               } else {
                   // Access own partition
                   tree.all_paths()
                       .iter()
                       .filter(|p| hash(p) % num_agents == agent_id)
                       .choose(&mut rand::thread_rng())
                       .unwrap()
               }
           }
           PathSelectionStrategy::Exclusive => {
               // Only own partition (minimal contention)
               tree.all_paths()
                   .iter()
                   .filter(|p| hash(p) % num_agents == agent_id)
                   .choose(&mut rand::thread_rng())
                   .unwrap()
           }
           PathSelectionStrategy::Weighted => {
               // Same as Partitioned (overlap controls the mix)
               select_with_overlap(tree, agent_id, num_agents, overlap)
           }
       }
   }
   ```

## Example Configurations

### Example 1: Maximum Metadata Contention (Shared FS Testing)
```yaml
target: "file:///mnt/lustre/bench/"
duration: 60s
concurrency: 16

distributed:
  shared_filesystem: true          # REQUIRED: explicit
  tree_creation_mode: "concurrent" # REQUIRED: all agents race-create
  path_selection: "random"         # Maximum contention (default)
  
  agents:
    - address: "node1:7761"
      id: "agent-1"
    - address: "node2:7761"
      id: "agent-2"
    - address: "node3:7761"
      id: "agent-3"
    - address: "node4:7761"
      id: "agent-4"

prepare:
  directory_structure:
    width: 4
    depth: 3      # 84 directories
    files_per_dir: 100
    distribution: "bottom"

workload:
  - op: mkdir
    path: "/"
    weight: 25
  - op: rmdir
    path: "/"
    recursive: false
    weight: 25
  - op: list
    path: "/"
    weight: 30
  - op: stat
    path: "/"
    weight: 20

# Result: 4 agents × 16 threads = 64 workers all hitting same 84 directories
# Tests realistic high-contention shared filesystem metadata performance
```

### Example 2: Reduced Contention (Partitioned Access)
```yaml
# Same as above but with reduced contention
distributed:
  shared_filesystem: true
  tree_creation_mode: "concurrent"
  path_selection: "partitioned"    # Agents prefer different dirs
  partition_overlap: 0.2           # 20% chance of collision

# Result: Each agent mostly works in own partition, 20% overlap
# Tests shared FS with realistic workload distribution
```

### Example 3: Minimal Contention (Exclusive Partitions)
```yaml
# Baseline for comparison
distributed:
  shared_filesystem: true
  tree_creation_mode: "concurrent"
  path_selection: "exclusive"      # Each agent has own directories

# Result: Minimal contention, establishes performance baseline
```

### Example 4: Isolated Testing (Independent Trees)
```yaml
# Each agent in separate namespace
distributed:
  shared_filesystem: false         # REQUIRED: explicit
  tree_creation_mode: "isolated"   # REQUIRED: agent-1/, agent-2/ trees
  # path_selection not used (each agent has own tree)

# Result: No contention, tests independent filesystem performance
```

### Example 5: Coordinator Mode (Pre-created Tree)
```yaml
# Single-threaded tree creation, then distributed workload
distributed:
  shared_filesystem: true
  tree_creation_mode: "coordinator"  # Controller creates, agents skip
  path_selection: "random"           # Maximum workload contention

# Result: Clean prepare phase, then high-contention workload
# Good for very large trees or cost-sensitive cloud storage
```

### Example 6: Tunable Contention (Weighted Strategy)
```yaml
distributed:
  shared_filesystem: true
  tree_creation_mode: "concurrent"
  path_selection: "weighted"
  partition_overlap: 0.4            # 40% chance to access others

# Result: 60% own partition, 40% others - tunable sweet spot
```

## Testing Strategy

1. **Unit tests**: ✅ Already have 10 tests for DirectoryTree logic
2. **Integration test**: Single-node shared FS simulation with path selection
3. **Distributed test**: Multi-agent with different contention levels
4. **Performance test**: Measure contention impact (random vs partitioned vs exclusive)
5. **Stress test**: 16 agents × 16 threads = 256 workers on small tree

## Next Steps

1. ✅ Implement DirectoryTree module (DONE)
2. ✅ Add comprehensive tests (DONE)
3. ✅ Fix width indexing bug (DONE)
4. ✅ **Finalize shared FS design** (DONE)
5. ⏳ Add config structs (TreeCreationMode, PathSelectionStrategy)
6. ⏳ Integrate directory_structure into PrepareConfig
7. ⏳ Implement tree creation in prepare phase
8. ⏳ Implement path selection in workload
9. ⏳ Add distributed mode support
10. ⏳ Create test configs

## Design Decisions (APPROVED)

1. ✅ **Option 3 (Concurrent)** as primary approach
2. ✅ **No auto-detection** - explicit `shared_filesystem` required
3. ✅ **No default for `tree_creation_mode`** - forces explicit choice
4. ✅ **Path selection strategies** for runtime contention control
5. ✅ **Tunable overlap** via `partition_overlap` parameter
6. ❌ **No staggered start times** (not needed, may reconsider later)

## Resolved Questions## Resolved Questions

1. **Tree creation strategy?**
   - ✅ Concurrent (Option 3) - idempotent race-safe creation
   - User can choose isolated/coordinator/concurrent explicitly

2. **Auto-detection of shared_filesystem?**
   - ✅ NO - must be explicitly set to true or false
   - Avoids surprises, forces intentional configuration

3. **Default for tree_creation_mode?**
   - ✅ NO default - must be explicitly specified
   - Makes configuration clear and intentional

4. **How to control contention?**
   - ✅ Path selection strategies (random/partitioned/exclusive/weighted)
   - Allows testing from max contention to minimal contention

5. **How to handle cleanup with shared trees?**
   - Option A: Only coordinator cleans up
   - Option B: Last agent to finish cleans up (requires coordination)
   - Option C: Manual cleanup (`--no-cleanup` flag) - **RECOMMENDED**
   - Decision: Use `--no-cleanup` for shared trees, manual cleanup between runs

6. **Should file creation also be shared or isolated?**
   - Shared tree + isolated files: agents create different files in shared dirs
   - Configuration handled by path_selection strategy
   - `random`: May create/delete each other's files (realistic high contention)
   - `exclusive`: Each agent only touches own files (controlled testing)

## Related Work

- **rdf-bench**: Java implementation with width/depth model (our inspiration)
- **IOR**: MPI-based, uses coordinator model for metadata tests
- **mdtest**: Focuses on concurrent metadata operations (like our Option 3)
- **vdbench**: Uses "anchors" for directory trees, single-threaded creation
- **Lustre lfs_test**: Parallel metadata benchmark with similar contention controls

## Performance Expectations

Based on typical shared filesystem behavior:

| Configuration | Expected Throughput | Use Case |
|---------------|-------------------|----------|
| `exclusive` + local disks | ~50K ops/s/agent | Baseline |
| `exclusive` + NFS | ~20K ops/s/agent | Minimal contention |
| `partitioned` (30% overlap) | ~15K ops/s/agent | Moderate contention |
| `random` + 4 agents | ~5K ops/s/agent | High contention (realistic) |
| `random` + 16 agents | ~2K ops/s/agent | Extreme contention |

*Note: Actual numbers vary by filesystem, hardware, and tree size*
