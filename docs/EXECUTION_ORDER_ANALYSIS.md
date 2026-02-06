# Current vs Proposed Execution Order

**Created**: February 4, 2026  
**Status**: Analysis Document

---

## Current Execution Order (v0.8.24)

```
1. SSH Deployment (if enabled)
   └─ Deploy Docker containers to remote hosts (2s wait)

2. Config Parsing
   └─ Read and parse YAML config

3. Pre-flight Validation (v0.8.23+) ✅ CORRECT LOCATION
   ├─ Connect to all agents (parallel)
   ├─ Distributed config validation
   └─ Run pre-flight validation on all agents (sequential, one at a time)
   
4. Calculate Start Time
   └─ Set coordinated start time = now + 10s (coordinated) + user_delay

5. Spawn Agent Streams (parallel)
   └─ Start execute_workload RPC for all agents

6. Agent Processing (each agent independently)
   ├─ Receive WorkloadRequest with config
   ├─ Validate config (basic checks)
   ├─ 🚨 CREATE TREE MANIFEST (30-90s for 331k dirs) ← BLOCKS HERE
   ├─ Transition to Running state
   ├─ Start LiveStats reporting
   ├─ Prepare Phase (if configured)
   │  ├─ List existing objects (if skip_verification=false)
   │  ├─ Create directories (finalize_tree_with_mkdir)
   │  └─ Create files (parallel workers)
   ├─ Wait for start_time (countdown)
   ├─ Execute Phase (workload operations)
   └─ Cleanup Phase (if configured)

7. Controller Aggregation
   └─ Collect LiveStats from all agents, display progress
```

### Problem Areas

1. **TreeManifest Creation Happens After Stream Start**
   - Controller sends execute_workload → agents receive config
   - Agents immediately start creating tree manifest (30-90s)
   - **BUT** LiveStats reporting starts AFTER tree creation completes
   - Result: 30-90s silence → h2 timeout (FIXED by progress reporting in v0.8.24a)

2. **Pre-flight Validation Order is CORRECT** ✅
   - Pre-flight runs AFTER connection but BEFORE streams spawn
   - This is the logical place (validates before expensive operations)

---

## Your Suggested Order

```
1. SSH Deployment (if enabled)
2. Config Parsing
3. Initial Handshake (connect to agents)
4. Pre-flight Validation ✅ (already here!)
5. Countdown Timer (if configured)
6. Prepare Phase
   ├─ TreeManifest creation
   └─ File creation
7. Execute Phase (workload)
8. Cleanup Phase
```

### Analysis

**Your suggestion is already implemented!** The current order is:

1. ✅ **Connect to agents** (line 1382-1399)
2. ✅ **Pre-flight validation** (line 1421-1444) - runs AFTER handshake, BEFORE streams
3. ✅ **Countdown timer** (line 1344: `start_time = now + coordinated_delay + user_delay`)
4. ✅ **Prepare phase** (happens on agent after validation)

The issue is **NOT the order** - the issue is:
- TreeManifest creation (30-90s) happens **inside** prepare phase
- Old code: No progress reporting during tree generation → timeout
- **FIXED in v0.8.24a**: Progress every 5000 dirs → no timeout

---

## Why TreeManifest Creation Can't Move Earlier

TreeManifest generation **must** happen on each agent because:

1. **Config-dependent**: Needs directory_structure config (width/depth/files_per_dir)
2. **Agent-specific**: Each agent calculates its file assignments (agent 1 gets files 0-15M, agent 2 gets 16-32M, etc.)
3. **Prepare-phase operation**: Logically part of "preparing the test data structure"

**Options**:

### Option A: Keep Current Order with Progress Reporting (v0.8.24a) ✅ IMPLEMENTED
- TreeManifest creation stays in prepare phase
- Progress updates every 5000 dirs prevent timeout
- **Pros**: No architectural changes, minimal risk
- **Cons**: Still takes 30-90s for 331k dirs

### Option B: Move Tree Generation to Pre-flight (risky architectural change)
- Pre-flight creates tree manifest and validates it exists
- Prepare phase skips tree creation if manifest cached
- **Pros**: Front-loads expensive operation to pre-flight
- **Cons**: 
  - Adds 30-90s to pre-flight (defeats "quick validation" purpose)
  - Requires caching manifest across RPC boundary
  - More complex error handling

### Option C: Reduce Tree Complexity for Testing (RECOMMENDED) ✅
- Use simpler configs to validate fixes work
- Progression: depth=2 (576 dirs, ~1s) → depth=3 (13k dirs, ~5s) → depth=4 (331k dirs, ~90s)
- **Pros**: Validates incrementally, reduces risk
- **Cons**: None - this is best practice

---

## Recommended Testing Strategy

### Phase 1: Validate Fixes Work (depth=2)
**Config**: `4host-test_SIMPLE.yaml`
- Width=24, Depth=2 → **576 directories** (tree generation ~1-2 seconds)
- 111,168 files × 8MB = **~889 GB total**
- **Expected**: No timeout, completes in ~5-10 minutes

### Phase 2: Stress Test (depth=3)
**Config**: `4host-test_MEDIUM.yaml`
- Width=24, Depth=3 → **13,824 directories** (tree generation ~5-10 seconds)
- 2,668,032 files × 8MB = **~21 TB total**
- **Expected**: Progress updates visible, completes in ~1-2 hours

### Phase 3: Production Scale (depth=4)
**Config**: `4host-test_config-corrected.yaml`
- Width=24, Depth=4 → **331,776 directories** (tree generation ~30-90 seconds)
- 64,032,768 files × 8MB = **~500 TB total**
- **Expected**: Progress updates every 1-2s, completes in ~10-20 hours

---

## Timeline Comparison

### Old Code (v0.8.23)
```
0s:  Connect to agents
5s:  Pre-flight validation complete
15s: Spawn streams
16s: Agents receive config
16s: Agents start tree generation (SILENT)
106s: 🔥 h2 TIMEOUT - no progress updates for 90s
```

### Fixed Code (v0.8.24a)
```
0s:   Connect to agents
5s:   Pre-flight validation complete
15s:  Spawn streams
16s:  Agents receive config
16s:  Agents start tree generation (WITH PROGRESS)
18s:  "Tree generation progress: 5000/331776 (1.5%)"
20s:  "Tree generation progress: 10000/331776 (3.0%)"
22s:  "Tree generation progress: 15000/331776 (4.5%)"
...
106s: "Tree generation progress: 331776/331776 (100%)"
107s: Prepare phase starts (file creation)
```

### With SIMPLE Config (depth=2)
```
0s:  Connect to agents
5s:  Pre-flight validation complete
15s: Spawn streams
16s: Agents receive config
16s: Agents start tree generation (WITH PROGRESS)
17s: "Tree generation progress: 576/576 (100%)" ✅ Complete!
18s: Prepare phase starts (file creation)
```

---

## Conclusion

1. **Execution order is already optimal** - pre-flight runs after handshake, before prepare
2. **TreeManifest creation must stay in prepare phase** - it's config-dependent and agent-specific
3. **Fix is correct** - progress reporting prevents timeout
4. **Recommendation**: Test with SIMPLE config first (depth=2) to validate fixes work

**Next client session**: Start with `4host-test_SIMPLE.yaml` → verify no timeout → proceed to MEDIUM → then FULL.
