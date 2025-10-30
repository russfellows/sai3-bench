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

## Recommended Approach

**Hybrid Strategy**: Support both modes with configuration flag

### Configuration Schema
```yaml
distributed:
  shared_filesystem: bool     # Default: auto-detect from target URI scheme
                              # true for file://, false for s3:// (inverted from current logic!)
  
  tree_creation_mode: string  # "isolated" | "coordinator" | "concurrent"
                              # Default: "isolated" for local, "coordinator" for shared
```

### Decision Matrix

| Target Backend | `shared_filesystem` | Default `tree_creation_mode` | Behavior |
|----------------|---------------------|------------------------------|----------|
| `file://` (NFS/Lustre) | `true` | `coordinator` | Controller creates, agents skip |
| `file://` (local disks) | `false` | `isolated` | Each agent creates own tree |
| `s3://` / `gs://` / `az://` | `true` | `concurrent` | All agents create (idempotent) |
| `direct://` | `false` | `isolated` | Each agent creates own tree |

### Implementation Plan

1. **Add config fields** to `DistributedConfig`:
   ```rust
   pub struct DistributedConfig {
       // ...existing fields...
       
       /// Whether target filesystem is shared across agents
       /// Default: auto-detect (true for file://, false for s3://|direct://)
       #[serde(default)]
       pub shared_filesystem: Option<bool>,
       
       /// How to handle directory tree creation on shared filesystems
       /// - "isolated": Each agent creates separate tree under agent-{id}/
       /// - "coordinator": Controller creates tree, agents skip prepare
       /// - "concurrent": All agents create tree (idempotent operations)
       #[serde(default)]
       pub tree_creation_mode: Option<TreeCreationMode>,
   }
   
   #[derive(Debug, Deserialize, Clone)]
   #[serde(rename_all = "lowercase")]
   pub enum TreeCreationMode {
       Isolated,
       Coordinator,
       Concurrent,
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

4. **Add helpers**:
   - `is_shared_filesystem(target: &str) -> bool`: Auto-detect based on URI scheme
   - `default_tree_creation_mode(shared: bool) -> TreeCreationMode`: Smart defaults

## Testing Strategy

1. **Unit tests**: Already have 10 tests for DirectoryTree logic
2. **Integration test**: Single-node shared FS simulation
3. **Distributed test**: Multi-agent with different modes
4. **Performance test**: Compare isolated vs concurrent creation on Lustre/NFS

## Next Steps

1. ✅ Implement DirectoryTree module (DONE)
2. ✅ Add comprehensive tests (DONE)
3. ✅ Fix width indexing bug (DONE)
4. 🔄 **Discuss shared FS design** (IN PROGRESS)
5. ⏳ Integrate into PrepareConfig
6. ⏳ Implement tree creation in prepare phase
7. ⏳ Add distributed mode support
8. ⏳ Create example configs

## Open Questions

1. **Should we default to "concurrent" for cloud storage?**
   - Pro: Tests realistic multi-client behavior
   - Con: Higher API request costs during prepare

2. **How to handle cleanup with shared trees?**
   - Option A: Only coordinator cleans up
   - Option B: Last agent to finish cleans up
   - Option C: Manual cleanup (`--no-cleanup` flag)

3. **Should file creation also be shared or isolated?**
   - Shared tree + isolated files: agents create different files in shared dirs
   - Shared tree + shared files: agents may read/write/delete each other's files
   - Configuration: `file_ownership: "isolated" | "shared"`

## Related Work

- **rdf-bench**: Java implementation with width/depth model (our inspiration)
- **IOR**: MPI-based, uses coordinator model for metadata tests
- **mdtest**: Focuses on concurrent metadata operations (like Option 3)
- **vdbench**: Uses "anchors" for directory trees, single-threaded creation
