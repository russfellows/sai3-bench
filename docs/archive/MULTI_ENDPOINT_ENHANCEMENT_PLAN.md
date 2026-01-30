# Multi-Endpoint Enhancement Plan for sai3-bench

**Date**: January 29, 2026  
**Author**: AI Analysis  
**Status**: Design Proposal  
**Related**: [Issue tracking multi-endpoint support for distributed agents]

---

## Executive Summary

**CURRENT STATE**: sai3-bench can run distributed workloads (controller + agents), but each agent targets a single storage endpoint/mount point.

**USER REQUIREMENT**: Run 4 test hosts (agents), each targeting 2 different storage IP addresses from an 8-IP storage system (4 × 2 = 8 total endpoints).

**GOOD NEWS**: The underlying **s3dlio library v0.9.14+ already supports multi-endpoint** load balancing with `MultiEndpointStore`. We just need to expose this capability in sai3-bench configuration.

**RECOMMENDATION**: Add multi-endpoint support to sai3-bench with **both global and per-agent configuration options**.

---

## Current Capabilities Analysis

### ✅ What Works Today

1. **s3dlio Multi-Endpoint Support (v0.9.14+)**
   - `MultiEndpointStore` with round-robin and least-connections strategies
   - Per-endpoint statistics tracking (requests, bytes, errors, latency)
   - Supports multiple S3/Azure/GCS endpoints

2. **sai3-bench Distributed Architecture**
   - Controller coordinates multiple agents
   - Each agent runs independent workload
   - Consolidated results with histogram merging

3. **dl-driver Reference Implementation**
   - Already uses s3dlio multi-endpoint support
   - Has `endpoint_uris` config field (Vec<String>)
   - Has `load_balance_strategy` config field

### ❌ Current Limitations

1. **No Multi-Endpoint Config in sai3-bench**
   - Current config only has single `target` field
   - No way to specify multiple URIs per agent
   - No load balancing configuration

2. **File:// Multi-Mount Point Support Unclear**
   - s3dlio `MultiEndpointStore` works for S3/Azure/GCS
   - **NEEDS VERIFICATION**: Can it load balance across multiple file:// URIs?
   - If not, may need enhancement in s3dlio first

3. **No Per-Agent Endpoint Assignment**
   - Can't assign specific endpoints to specific agents in distributed mode
   - All agents would use same endpoint list (not ideal for your scenario)

---

## Proposed Solution: Multi-Level Configuration

### Design Philosophy

Support **three levels** of endpoint configuration:

1. **Global (Workload-Level)**: All agents use same endpoint list
2. **Per-Agent Override**: Specific agents get specific endpoint lists
3. **Hybrid**: Global default + per-agent overrides

This mirrors existing sai3-bench patterns (e.g., per-agent concurrency overrides).

---

## Configuration Schema Changes

### Option 1: Global Multi-Endpoint (Simple)

All agents target the same set of endpoints with load balancing:

```yaml
# Global multi-endpoint configuration (all agents share these endpoints)
target: "s3://bucket/data/"  # Base bucket/path (optional fallback)

multi_endpoint:
  enabled: true
  strategy: round_robin  # or least_connections
  endpoints:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
    - s3://192.168.1.12:9000/bucket/
    - s3://192.168.1.13:9000/bucket/
    - s3://192.168.1.14:9000/bucket/
    - s3://192.168.1.15:9000/bucket/
    - s3://192.168.1.16:9000/bucket/
    - s3://192.168.1.17:9000/bucket/

duration: 60s
concurrency: 32

workload:
  - op: get
    path: "objects/*"
    weight: 70
  - op: put
    path: "objects/"
    object_size: 1048576
    weight: 30
```

**Behavior**: All 4 agents round-robin across all 8 endpoints (may not be optimal for your use case).

---

### Option 2: Per-Agent Endpoint Assignment (Your Use Case)

Each agent targets a specific subset of endpoints:

```yaml
target: "s3://bucket/data/"  # Fallback if agent doesn't specify endpoints

duration: 60s
concurrency: 32

distributed:
  agents:
    # Agent 1: Targets endpoints 1 & 2
    - address: "testhost1:7761"
      id: agent1
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.10:9000/bucket/
          - s3://192.168.1.11:9000/bucket/
    
    # Agent 2: Targets endpoints 3 & 4
    - address: "testhost2:7761"
      id: agent2
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.12:9000/bucket/
          - s3://192.168.1.13:9000/bucket/
    
    # Agent 3: Targets endpoints 5 & 6
    - address: "testhost3:7761"
      id: agent3
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.14:9000/bucket/
          - s3://192.168.1.15:9000/bucket/
    
    # Agent 4: Targets endpoints 7 & 8
    - address: "testhost4:7761"
      id: agent4
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.16:9000/bucket/
          - s3://192.168.1.17:9000/bucket/

workload:
  - op: get
    path: "objects/*"
    weight: 70
  - op: put
    path: "objects/"
    object_size: 1048576
    weight: 30
```

**Behavior**: Each agent only uses its assigned 2 endpoints. Perfect load distribution: 4 agents × 2 endpoints/agent = 8 endpoints total.

---

### Option 3: Hybrid (Global Default + Per-Agent Override)

Global endpoint list with optional per-agent overrides:

```yaml
# Global default (used by agents without override)
multi_endpoint:
  strategy: round_robin
  endpoints:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
    - s3://192.168.1.12:9000/bucket/
    - s3://192.168.1.13:9000/bucket/

duration: 60s
concurrency: 32

distributed:
  agents:
    - address: "testhost1:7761"
      id: agent1
      # Uses global endpoints (4 endpoints)
    
    - address: "testhost2:7761"
      id: agent2
      # Override: Only use these 2 endpoints
      multi_endpoint:
        endpoints:
          - s3://192.168.1.14:9000/bucket/
          - s3://192.168.1.15:9000/bucket/
    
    - address: "testhost3:7761"
      id: agent3
      # Uses global endpoints (4 endpoints)

workload:
  - op: get
    path: "objects/*"
    weight: 70
```

**Behavior**: agent1 and agent3 use global 4 endpoints, agent2 uses its specific 2 endpoints.

---

## File System Multi-Mount Point Support

### Current s3dlio Behavior (NEEDS VERIFICATION)

Looking at s3dlio's `MultiEndpointStore` implementation:

```rust
// From s3dlio/src/multi_endpoint.rs
pub struct MultiEndpointStore {
    endpoints: Vec<Arc<dyn ObjectStore>>,
    strategy: LoadBalanceStrategy,
    // ...
}
```

**Key Question**: Can you do this?

```rust
let store = MultiEndpointStore::new(
    vec![
        "file:///mnt/nfs1/data/".to_string(),
        "file:///mnt/nfs2/data/".to_string(),
    ],
    LoadBalanceStrategy::RoundRobin,
    None
)?;
```

**Expected Behavior**: Should work! Each `file://` URI creates a separate `FileSystemObjectStore`, and `MultiEndpointStore` round-robins between them.

**Caveat**: All file paths must be identical across mounts. Example:
- Mount 1: `/mnt/nfs1/data/file001.dat`
- Mount 2: `/mnt/nfs2/data/file001.dat`

This means your 8 NFS mount points must present the **same namespace** (same files accessible from each mount).

### Testing Recommendation

Before implementing in sai3-bench, create a simple test:

```rust
// Test file: s3dlio/tests/test_multi_endpoint_file.rs
use s3dlio::multi_endpoint::{MultiEndpointStore, LoadBalanceStrategy};

#[tokio::test]
async fn test_multi_mount_nfs() {
    let store = MultiEndpointStore::new(
        vec![
            "file:///mnt/nfs1/".to_string(),
            "file:///mnt/nfs2/".to_string(),
        ],
        LoadBalanceStrategy::RoundRobin,
        None,
    ).unwrap();
    
    // Verify both mounts are used
    // ...
}
```

If this doesn't work, we may need to enhance s3dlio's `FileSystemObjectStore` first.

---

## Implementation Plan

### Phase 1: Verification & Testing (1-2 days)

1. **Verify s3dlio file:// multi-endpoint support**
   - Create test with 2 local directories: `/tmp/mount1/`, `/tmp/mount2/`
   - Use `MultiEndpointStore` to round-robin between them
   - Verify per-endpoint stats tracking works

2. **Document findings**
   - If works: Proceed to Phase 2
   - If doesn't work: File issue in s3dlio, implement fix there first

### Phase 2: sai3-bench Config Schema (2-3 days)

1. **Add Config structs** (src/config.rs):

```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MultiEndpointConfig {
    /// Enable multi-endpoint mode (default: false)
    #[serde(default)]
    pub enabled: bool,
    
    /// Load balancing strategy (default: round_robin)
    /// Values: round_robin, least_connections
    #[serde(default = "default_load_balance_strategy")]
    pub strategy: String,
    
    /// List of endpoint URIs
    pub endpoints: Vec<String>,
}

fn default_load_balance_strategy() -> String {
    "round_robin".to_string()
}

// In Config struct:
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    // ... existing fields ...
    
    /// Global multi-endpoint configuration (v0.8.22+)
    /// All agents use these endpoints unless overridden per-agent
    #[serde(default)]
    pub multi_endpoint: Option<MultiEndpointConfig>,
    
    // ... rest of fields ...
}

// In DistributedAgentConfig struct:
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DistributedAgentConfig {
    pub address: String,
    pub id: Option<String>,
    
    // ... existing fields ...
    
    /// Per-agent multi-endpoint override (v0.8.22+)
    /// If specified, this agent uses these endpoints instead of global
    #[serde(default)]
    pub multi_endpoint: Option<MultiEndpointConfig>,
}
```

2. **Add validation**:
   - Error if both `target` and `multi_endpoint.enabled=true` are set (ambiguous)
   - Error if `multi_endpoint.enabled=true` but `endpoints` is empty
   - Warn if endpoint list length doesn't evenly divide by agent count

3. **Update documentation**:
   - CONFIG_SYNTAX.md: Add multi-endpoint section
   - DISTRIBUTED_TESTING_GUIDE.md: Add multi-endpoint examples

### Phase 3: Workload Integration (3-4 days)

1. **Modify store creation** (src/workload.rs):

```rust
pub fn create_store_for_config(config: &Config, agent_config: Option<&DistributedAgentConfig>) 
    -> Result<Box<dyn ObjectStore>> 
{
    // Priority: per-agent > global > target
    if let Some(agent) = agent_config {
        if let Some(ref multi_ep) = agent.multi_endpoint {
            return create_multi_endpoint_store(multi_ep);
        }
    }
    
    if let Some(ref multi_ep) = config.multi_endpoint {
        if multi_ep.enabled {
            return create_multi_endpoint_store(multi_ep);
        }
    }
    
    // Fallback: single target
    if let Some(ref target) = config.target {
        return create_store_for_uri(target);
    }
    
    bail!("No target or multi_endpoint configuration specified");
}

fn create_multi_endpoint_store(config: &MultiEndpointConfig) -> Result<Box<dyn ObjectStore>> {
    use s3dlio::multi_endpoint::{MultiEndpointStore, LoadBalanceStrategy};
    
    let strategy = match config.strategy.as_str() {
        "round_robin" => LoadBalanceStrategy::RoundRobin,
        "least_connections" => LoadBalanceStrategy::LeastConnections,
        _ => bail!("Invalid load_balance_strategy: {}", config.strategy),
    };
    
    let store = MultiEndpointStore::new(
        config.endpoints.clone(),
        strategy,
        None, // No per-endpoint thread config for now
    )?;
    
    Ok(Box::new(store))
}
```

2. **Update prepare phase**:
   - When multi-endpoint enabled, prepare must ensure objects exist on **all** endpoints
   - Option 1: Write to each endpoint separately (ensures replication)
   - Option 2: Write through `MultiEndpointStore` (assumes storage system replicates)
   - Recommend Option 2 for simplicity, document assumption

3. **Update cleanup phase**:
   - Similar to prepare: delete through `MultiEndpointStore`
   - Per-endpoint cleanup not necessary (storage system handles replication)

### Phase 4: Statistics & Logging (2-3 days)

1. **Per-endpoint stats in results**:
   - `MultiEndpointStore` already tracks per-endpoint stats
   - Add new section to results.tsv: `[endpoint_stats]`
   - Show requests/bytes/errors per endpoint

2. **Console output enhancements**:
   - Display endpoint list during startup
   - Show load distribution during execution
   - Example: `Endpoints: 8 (2 per agent, round-robin)`

3. **Perf-log integration**:
   - Add per-endpoint columns to perf_log.tsv
   - Track which endpoint served each operation (if feasible)

### Phase 5: Testing & Documentation (2-3 days)

1. **Create test configs**:
   - `tests/configs/multi_endpoint_global.yaml`
   - `tests/configs/multi_endpoint_per_agent.yaml`
   - `tests/configs/multi_endpoint_nfs.yaml`

2. **Integration tests**:
   - Test with local file:// endpoints (simulated NFS)
   - Test with S3 endpoints (if available)
   - Test per-agent override behavior

3. **Update documentation**:
   - README.md: Add multi-endpoint feature
   - CHANGELOG.md: Document v0.8.22 release
   - Examples in docs/

---

## Alternative: Quick Workaround (If You Need This Today)

If you need this capability **immediately** before the feature is implemented:

### Workaround 1: Run Multiple Agent Instances Per Host

Each host runs 2 agent processes, each targeting a different endpoint:

```bash
# On testhost1:
./sai3bench-agent --listen 0.0.0.0:7761 --id agent1a &
./sai3bench-agent --listen 0.0.0.0:7762 --id agent1b &

# On testhost2:
./sai3bench-agent --listen 0.0.0.0:7761 --id agent2a &
./sai3bench-agent --listen 0.0.0.0:7762 --id agent2b &

# ... etc for testhost3 and testhost4
```

Then use 2 separate configs:

**config_endpoints_1_2.yaml** (for agents *a):
```yaml
target: "s3://192.168.1.10:9000/bucket/"
# ... workload ...
```

**config_endpoints_3_4.yaml** (for agents *b):
```yaml
target: "s3://192.168.1.11:9000/bucket/"
# ... workload ...
```

Run controller twice (or use 2 controllers):
```bash
./sai3bench-ctl --agents testhost1:7761,testhost2:7761,testhost3:7761,testhost4:7761 \
  run --config config_endpoints_1_2.yaml &

./sai3bench-ctl --agents testhost1:7762,testhost2:7762,testhost3:7762,testhost4:7762 \
  run --config config_endpoints_3_4.yaml &
```

**Pros**: Works with current code, no changes needed  
**Cons**: 8 agent processes, 2 configs, manual result merging

### Workaround 2: Use dl-driver Instead

dl-driver **already has multi-endpoint support** (`endpoint_uris` field):

```yaml
# dl-driver config (DLIO-compatible format)
dataset:
  endpoint_uris:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
  load_balance_strategy: round_robin
  
  data_folder: /data/
  num_files_train: 1000
  # ... etc ...
```

**Pros**: Feature already exists  
**Cons**: Different config format, AI/ML focused, may not fit your exact use case

---

## Recommended Approach

**For your immediate need (4 hosts, 8 endpoints, 2 endpoints/host)**:

1. **SHORT TERM** (this week): Use Workaround 1 (multiple agents per host)
2. **LONG TERM** (next release): Implement full multi-endpoint support in sai3-bench

**Implementation Priority**:

1. ✅ **Phase 1** (verification): Critical - must confirm file:// multi-endpoint works
2. ✅ **Phase 2** (config schema): High priority - enables feature
3. ✅ **Phase 3** (workload integration): High priority - makes it work
4. ⚠️ **Phase 4** (statistics): Medium priority - nice to have
5. ⚠️ **Phase 5** (testing/docs): High priority - ensures quality

**Estimated Total Effort**: 10-14 days of focused development

---

## Questions to Resolve

1. **File system multi-mount assumption**:
   - Do all 8 mount points present identical file namespaces?
   - If not, we need a different approach (object-level endpoint assignment)

2. **Storage system replication**:
   - Does your storage system automatically replicate across all 8 IPs?
   - Or are they independent storage nodes?
   - This affects prepare/cleanup strategy

3. **Load balancing preference**:
   - Round-robin (simple, predictable)
   - Least-connections (better for heterogeneous endpoints)
   - Your use case?

4. **Performance goal**:
   - What throughput are you targeting?
   - This helps size concurrency and validate results

---

## Next Steps

### For You (User)

1. **Clarify requirements**:
   - Answer questions above
   - Confirm file namespace identity across mounts
   - Specify load balancing preference

2. **Test s3dlio multi-endpoint with file://**:
   ```bash
   cd /home/eval/Documents/Code/s3dlio
   # Create test with 2 local directories
   # Verify MultiEndpointStore works with file:// URIs
   ```

3. **Choose approach**:
   - Workaround now + feature later?
   - Wait for feature implementation?
   - Use dl-driver instead?

### For Implementation (if proceeding)

1. Start with Phase 1 verification
2. Create feature branch: `feature/multi-endpoint-support`
3. Implement Phases 2-3 (minimal viable feature)
4. Test with your actual setup (4 hosts, 8 endpoints)
5. Refine based on feedback
6. Add Phases 4-5 (polish)
7. Merge to main, tag v0.8.22

---

## Appendix: Config Examples

### Example 1: Simple Global Multi-Endpoint (All Agents Share)

```yaml
# All 4 agents round-robin across all 8 endpoints
multi_endpoint:
  enabled: true
  strategy: round_robin
  endpoints:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
    - s3://192.168.1.12:9000/bucket/
    - s3://192.168.1.13:9000/bucket/
    - s3://192.168.1.14:9000/bucket/
    - s3://192.168.1.15:9000/bucket/
    - s3://192.168.1.16:9000/bucket/
    - s3://192.168.1.17:9000/bucket/

duration: 300s
concurrency: 64

workload:
  - op: get
    path: "data/*"
    weight: 70
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 30
```

### Example 2: Per-Agent Endpoint Assignment (Your Use Case)

```yaml
# Each agent gets 2 specific endpoints
duration: 300s
concurrency: 64

distributed:
  agents:
    - address: "testhost1:7761"
      id: agent1
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.10:9000/bucket/
          - s3://192.168.1.11:9000/bucket/
    
    - address: "testhost2:7761"
      id: agent2
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.12:9000/bucket/
          - s3://192.168.1.13:9000/bucket/
    
    - address: "testhost3:7761"
      id: agent3
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.14:9000/bucket/
          - s3://192.168.1.15:9000/bucket/
    
    - address: "testhost4:7761"
      id: agent4
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.16:9000/bucket/
          - s3://192.168.1.17:9000/bucket/

workload:
  - op: get
    path: "data/*"
    weight: 70
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 30
```

### Example 3: NFS Multi-Mount (File System)

```yaml
# Round-robin across 2 NFS mount points
multi_endpoint:
  enabled: true
  strategy: round_robin
  endpoints:
    - file:///mnt/nfs1/benchmark/
    - file:///mnt/nfs2/benchmark/

duration: 300s
concurrency: 32

workload:
  - op: get
    path: "data/*"
    weight: 100
```

**Assumption**: `/mnt/nfs1/benchmark/data/` and `/mnt/nfs2/benchmark/data/` contain identical files.

---

**End of Analysis**

This comprehensive plan should give you everything needed to decide on approach and implement multi-endpoint support in sai3-bench. The underlying s3dlio library already has the capability - we just need to expose it through configuration.
