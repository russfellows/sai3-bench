# Distributed Cleanup Design Principles

## CRITICAL: Shared Storage Object Distribution

**When `shared_filesystem: true`, each agent operates INDEPENDENTLY on its DETERMINISTIC subset of objects.**

### The Golden Rule: NO LISTING IN DISTRIBUTED MODE

**NEVER attempt to:**
- List all objects and distribute them across agents
- Build a "master list" that agents share
- Coordinate which agent deletes which object at runtime

**Instead:**
- Each agent uses the SAME deterministic algorithm to compute its subset
- The algorithm is: `object_index % num_agents == agent_id`
- This works for BOTH creation (prepare) AND deletion (cleanup)

### Why This Design is Correct

1. **Listing is Expensive**: On cloud storage with millions of objects, listing can take 30+ minutes
2. **Deterministic = No Coordination**: Each agent knows exactly which objects belong to it
3. **Idempotent**: Safe to run multiple times - agent only touches "its" objects
4. **Scalable**: Adding more agents doesn't require resharding or re-listing

### The Algorithm

```rust
// Agent 0 (of 2 agents) handles: 0, 2, 4, 6, 8, 10, ...
// Agent 1 (of 2 agents) handles: 1, 3, 5, 7, 9, 11, ...

for index in 0..total_count {
    if index % num_agents == agent_id {
        // This agent creates/deletes object at this index
        let uri = format!("{}prepared-{:08}.dat", base_uri, index);
        // CREATE or DELETE
    }
}
```

### Object Naming Convention

Objects MUST be named with sequential indices for modulo distribution to work:
- `prepared-00000000.dat` (agent 0)
- `prepared-00000001.dat` (agent 1)
- `prepared-00000002.dat` (agent 0)
- `prepared-00000003.dat` (agent 1)
- ...

### Cleanup Modes

#### cleanup_mode: tolerant (RECOMMENDED for shared storage)
- Ignore "not found" errors
- Perfect for distributed cleanup where object may have been deleted already
- Use when resuming interrupted cleanup operations

#### skip_verification: true (REQUIRED for cleanup-only without listing)
- DO NOT list existing objects
- Generate the list of objects this agent WOULD have created
- Delete them using the deterministic algorithm
- Ignore "not found" errors (via `cleanup_mode: tolerant`)

#### skip_verification: false (USE WITH CAUTION)
- List existing objects FIRST
- Filter to objects matching this agent's modulo pattern
- Delete what was found
- WARNING: Listing may take 30+ minutes on cloud storage with large object counts!

### Cleanup-Only Workflow

To run cleanup without prepare or workload:

```yaml
duration: "0s"
workload: []

prepare:
  cleanup: true
  cleanup_mode: tolerant      # Ignore "not found" errors
  skip_verification: true     # DO NOT LIST - use deterministic algorithm
  ensure_objects:
    - base_uri: "s3://bucket/prefix/"
      count: 1000000           # Total objects across ALL agents
      size_spec: 2048
```

Each agent will:
1. **Skip listing** (because `skip_verification: true`)
2. **Generate its subset**: indices where `i % num_agents == agent_id`
3. **Delete deterministically**: `prepared-00000000.dat`, `prepared-00000002.dat`, ... (for agent 0 of 2)
4. **Ignore "not found"**: Some objects may already be deleted (via `cleanup_mode: tolerant`)

### Per-Agent Storage Mode

When `shared_filesystem: false`:
- Each agent operates in its own namespace: `agent-{id}/prepared-00000000.dat`
- No modulo distribution needed
- Each agent creates/deletes ALL objects in its namespace
- Listing is scoped to agent's namespace (smaller, faster)

### Common Mistakes to Avoid

❌ **WRONG**: "Let's list all objects and distribute them across agents"
- This requires expensive listing operation
- Defeats the purpose of distributed execution
- Not scalable

❌ **WRONG**: "Agent should only delete objects it created during THIS run"
- Cleanup-only mode has no prepare phase - nothing was created this run
- Must use deterministic algorithm to know what to delete

❌ **WRONG**: "Build PreparedObject list with obj.created=true from prepare phase"
- In cleanup-only mode, there is no prepare phase
- PreparedObject list doesn't exist
- Must generate deletion list from config

✅ **CORRECT**: "Each agent generates its deletion list using modulo algorithm"
- Same algorithm used for creation
- No coordination needed
- No listing required (with skip_verification: true)
- Scales to millions of objects

### Implementation Location

The deterministic deletion logic should be in:
- `src/prepare.rs`: Function that generates object URIs from config
- Uses same algorithm as prepare phase for consistency
- Takes: `PrepareConfig`, `agent_id`, `num_agents`
- Returns: List of URIs this agent should delete

### Example Scenario

**Setup**: 2 agents, 30 objects total, cleanup-only mode

**Agent 0 generates**:
```
file:///tmp/test/prepared-00000000.dat  (0 % 2 == 0) ✓
file:///tmp/test/prepared-00000002.dat  (2 % 2 == 0) ✓
file:///tmp/test/prepared-00000004.dat  (4 % 2 == 0) ✓
...
file:///tmp/test/prepared-00000028.dat  (28 % 2 == 0) ✓
```
Total: 15 objects

**Agent 1 generates**:
```
file:///tmp/test/prepared-00000001.dat  (1 % 2 == 1) ✓
file:///tmp/test/prepared-00000003.dat  (3 % 2 == 1) ✓
file:///tmp/test/prepared-00000005.dat  (5 % 2 == 1) ✓
...
file:///tmp/test/prepared-00000029.dat  (29 % 2 == 1) ✓
```
Total: 15 objects

**Result**: All 30 objects deleted, NO listing required, NO coordination needed!

### Testing

When testing distributed cleanup:
1. Verify each agent deletes ONLY its modulo subset
2. Verify total deletions == total objects
3. Verify NO listing calls were made (check s3dlio logs)
4. Verify cleanup completes in reasonable time (< 1 minute for local files)

### Summary

**The key insight**: Distributed cleanup is just the REVERSE of distributed prepare. Use the SAME deterministic algorithm, and each agent operates independently on its subset. NO listing, NO coordination, NO shared state!
