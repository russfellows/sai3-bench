# Multi-Endpoint Routing Implementation Analysis

**Date**: January 29, 2026  
**Status**: Statistics infrastructure complete, routing logic pending  
**Critical Decision Needed**: How to route operations to MultiEndpointStore based on `use_multi_endpoint` flag

---

## Executive Summary

We have successfully implemented complete per-endpoint statistics collection infrastructure. The final critical piece is implementing the routing logic that actually directs operations to use MultiEndpointStore when `use_multi_endpoint: true` is set.

This document analyzes the architectural challenge and presents implementation options without recommendation, allowing careful evaluation of trade-offs.

---

## Current State

### ✅ Completed (Statistics Infrastructure)

1. **Config Layer**: Added `use_multi_endpoint: bool` field to all OpSpec variants (Get, Put, List, Stat, Delete)
2. **Cache Infrastructure**: Created `MultiEndpointCache` type to track `Arc<MultiEndpointStore>` instances
3. **Store Creation**: Modified `create_multi_endpoint_store()` to return tuple `(Box<dyn ObjectStore>, Arc<MultiEndpointStore>)` for dual access
4. **Cache Population**: Updated `get_cached_store()` and all prepare functions to populate both caches
5. **Statistics Collection**: Implemented `collect_endpoint_stats()` that reads from multi_ep_cache and converts to display format
6. **Display**: Beautiful per-endpoint statistics with load distribution percentages

**Build Status**: ✅ Zero errors, zero warnings (ZERO_WARNINGS_POLICY compliant)

### ❌ Not Implemented (Routing Logic)

The `use_multi_endpoint` flag exists in config but is **not consulted during operation execution**. Operations currently use the existing global/per-agent multi-endpoint detection logic, which doesn't respect per-operation flags.

**Result**: When running benchmark with `use_multi_endpoint: true` on operations, they still use single-endpoint stores, so all endpoint statistics show 0.

---

## The Core Challenge

### Current Store Creation Flow

```rust
// In workload execution (simplified):
match op {
    OpSpec::Get { .. } => {
        let uri = resolve_uri();  // e.g., "file:///tmp/ep1/testdata/obj_123"
        
        // Calls get_object_cached()
        let bytes = get_object_cached(
            &uri,
            &store_cache,
            &multi_ep_cache,
            &config,
            agent_config
        ).await?;
        
        // Which calls get_cached_store()
        // Which checks: config.multi_endpoint.is_some()
        // Does NOT check: operation's use_multi_endpoint flag
    }
}
```

### The Routing Decision Point

We need to decide **at store creation time** whether to:
- Create a `MultiEndpointStore` (load balances across N endpoints)
- Create a regular single-endpoint store (uses URI directly)

**Key Insight**: The URI passed to `get_object_cached()` is already fully resolved (e.g., `file:///tmp/ep1/testdata/obj_123`). But for multi-endpoint, we need to:
1. Extract the path component (`testdata/obj_123`)
2. Combine with multi-endpoint configuration (4 endpoint roots)
3. Create MultiEndpointStore that can access all 4 endpoints

---

## Implementation Options

### Option 1: Thread `use_multi_endpoint` Through Call Chain

**Approach**: Add `use_multi_endpoint: bool` parameter to cached operation functions.

**Changes Required**:

```rust
// 1. Update signatures (5 functions):
fn get_object_cached(
    uri: &str,
    store_cache: &StoreCache,
    multi_ep_cache: &MultiEndpointCache,
    config: &Config,
    agent_config: Option<&AgentConfig>,
    use_multi_endpoint: bool,  // NEW
) -> Result<Bytes>

fn put_object_cached(..., use_multi_endpoint: bool) -> Result<()>
fn delete_object_cached(..., use_multi_endpoint: bool) -> Result<()>
fn list_objects_cached(..., use_multi_endpoint: bool) -> Result<Vec<String>>
fn stat_object_cached(..., use_multi_endpoint: bool) -> Result<u64>

// 2. Update get_cached_store signature:
fn get_cached_store(
    uri: &str,
    cache: &StoreCache,
    multi_ep_cache: &MultiEndpointCache,
    config: &Config,
    agent_config: Option<&AgentConfig>,
    use_multi_endpoint: bool,  // NEW
) -> Result<Arc<Box<dyn ObjectStore>>>

// 3. Update get_cached_store logic:
fn get_cached_store(..., use_multi_endpoint: bool) -> Result<...> {
    // Priority order for multi-endpoint decision:
    // 1. Operation-level flag (use_multi_endpoint parameter)
    // 2. Agent-level config (agent_config.multi_endpoint)
    // 3. Global config (config.multi_endpoint)
    
    let should_use_multi_ep = use_multi_endpoint && 
        (agent_config.and_then(|a| a.multi_endpoint.as_ref()).is_some() || 
         config.multi_endpoint.is_some());
    
    if should_use_multi_ep {
        // Extract path component from URI
        // Create MultiEndpointStore with path routing
        ...
    } else {
        // Create single-endpoint store (existing behavior)
        ...
    }
}

// 4. Update ALL operation match arms (6 places in workload.rs):
OpSpec::Get { use_multi_endpoint, .. } => {
    let bytes = get_object_cached(
        &uri,
        &store_cache,
        &multi_ep_cache,
        &config,
        agent_config,
        *use_multi_endpoint,  // Pass the flag
    ).await?;
}

OpSpec::Put { use_multi_endpoint, .. } => { ... }
OpSpec::List { use_multi_endpoint, .. } => { ... }
OpSpec::Stat { use_multi_endpoint, .. } => { ... }
OpSpec::Delete { use_multi_endpoint, .. } => { ... }
```

**URI Path Extraction Challenge**:

When `use_multi_endpoint: true`, we receive a URI like:
- `file:///tmp/ep1/testdata/obj_123`

We need to:
1. Detect which endpoint prefix it matches (`file:///tmp/ep1/`)
2. Extract the path component (`testdata/obj_123`)
3. Create MultiEndpointStore configured with all endpoints
4. Request will be routed by MultiEndpointStore's load balancing

**Code Pattern**:
```rust
if should_use_multi_ep {
    // Get multi-endpoint config
    let multi_ep = agent_config
        .and_then(|a| a.multi_endpoint.as_ref())
        .or(config.multi_endpoint.as_ref())
        .ok_or_else(|| anyhow!("use_multi_endpoint=true but no multi_endpoint config"))?;
    
    // Find matching endpoint prefix
    let path_component = extract_path_from_multi_endpoint_uri(uri, &multi_ep.endpoints)?;
    
    // Create MultiEndpointStore (will internally route requests)
    let (boxed_store, multi_store) = create_multi_endpoint_store(...)?;
    
    // Cache it
    // Return it
}
```

**Pros**:
- ✅ Clean separation: flag flows explicitly through call chain
- ✅ Type-safe: compiler enforces parameter threading
- ✅ Clear precedence: operation flag + config existence
- ✅ Consistent with existing parameter patterns

**Cons**:
- ❌ Requires updating 5 function signatures
- ❌ Requires updating ~20+ call sites across codebase
- ❌ More complex for distributed mode (agent.rs call sites)
- ❌ URI path extraction logic needed (which endpoint prefix matches?)

**Risk Assessment**: **LOW**
- Mechanical change (add parameter, pass through)
- Compiler catches all missing updates
- No silent failures

---

### Option 2: Encode `use_multi_endpoint` in URI or Cache Key

**Approach**: Modify URI or cache key to signal multi-endpoint routing intent.

**Variant 2A: URI Prefix Scheme**

```rust
// In operation execution:
OpSpec::Get { path, use_multi_endpoint, .. } => {
    let mut uri = resolve_uri(path);
    
    if *use_multi_endpoint {
        // Prepend special marker
        uri = format!("multi-ep:{}", uri);
    }
    
    let bytes = get_object_cached(&uri, ...).await?;
}

// In get_cached_store:
fn get_cached_store(uri: &str, ...) -> Result<...> {
    if uri.starts_with("multi-ep:") {
        let actual_uri = &uri[9..];  // Strip "multi-ep:" prefix
        // Create MultiEndpointStore
    } else {
        // Create single-endpoint store
    }
}
```

**Pros**:
- ✅ No signature changes
- ✅ Minimal call site changes

**Cons**:
- ❌ **MAJOR**: Violates URI scheme standards
- ❌ **MAJOR**: Magic string coupling ("multi-ep:" scattered across code)
- ❌ Debugging nightmare (URIs in logs are fake)
- ❌ Error messages will show fake URIs
- ❌ Fragile: easy to forget prefix somewhere

**Variant 2B: Context Object**

```rust
struct OperationContext {
    use_multi_endpoint: bool,
    agent_config: Option<AgentConfig>,
    // Could add more operation metadata later
}

fn get_object_cached(
    uri: &str,
    store_cache: &StoreCache,
    multi_ep_cache: &MultiEndpointCache,
    config: &Config,
    ctx: &OperationContext,  // NEW: bundles all context
) -> Result<Bytes>
```

**Pros**:
- ✅ Extensible: easy to add more operation metadata
- ✅ Cleaner signatures (one context parameter vs multiple)
- ✅ Groups related configuration

**Cons**:
- ❌ Still requires updating 5 function signatures
- ❌ Still requires updating ~20+ call sites
- ❌ More boilerplate (create context object at each call site)
- ❌ Doesn't solve the URI path extraction problem

**Risk Assessment**: **MEDIUM-HIGH**
- 2A is architecturally unsound (fake URIs)
- 2B is just Option 1 with extra wrapper

---

### Option 3: Dual Cache System with Operation-Aware Lookup

**Approach**: Instead of having `get_cached_store()` decide, have the cached operation functions do smart lookup.

**Current**: 
```
get_object_cached() 
  → get_cached_store()  // Makes routing decision
    → create single store OR MultiEndpointStore
```

**Proposed**:
```
get_object_cached(use_multi_endpoint: bool)
  → if use_multi_endpoint:
      lookup in multi_ep_cache (return existing MultiEndpointStore)
    else:
      lookup in store_cache (existing single-endpoint path)
```

**Implementation**:

```rust
async fn get_object_cached(
    uri: &str,
    store_cache: &StoreCache,
    multi_ep_cache: &MultiEndpointCache,
    config: &Config,
    agent_config: Option<&AgentConfig>,
    use_multi_endpoint: bool,
) -> Result<Bytes> {
    let store = if use_multi_endpoint {
        // Look up existing MultiEndpointStore from cache
        let multi_ep_config = agent_config
            .and_then(|a| a.multi_endpoint.as_ref())
            .or(config.multi_endpoint.as_ref())
            .ok_or_else(|| anyhow!("use_multi_endpoint=true but no config"))?;
        
        let cache_key = format!("multi_ep:{}:{}",
            multi_ep_config.strategy,
            multi_ep_config.endpoints.join(",")
        );
        
        // Get from multi_ep_cache (Arc<MultiEndpointStore>)
        // Wrap in Arc<Box<dyn ObjectStore>> for consistent return type
        get_or_create_multi_endpoint_store(
            &cache_key,
            store_cache,
            multi_ep_cache,
            multi_ep_config,
            config
        )?
    } else {
        // Existing single-endpoint path
        get_cached_store(uri, store_cache, multi_ep_cache, config, agent_config, false)?
    };
    
    // Extract path component for multi-endpoint routing
    let path = if use_multi_endpoint {
        extract_path_component(uri, config, agent_config)?
    } else {
        uri
    };
    
    store.get(path).await?
}
```

**Pros**:
- ✅ Routing decision at operation level (closer to source of truth)
- ✅ Clear separation: multi-endpoint path vs single-endpoint path
- ✅ Easier to reason about flow

**Cons**:
- ❌ Duplicates logic across 5 cached functions
- ❌ Still requires path extraction logic
- ❌ Inconsistent with current architecture (get_cached_store does routing)
- ❌ Harder to maintain (routing logic in multiple places)

**Risk Assessment**: **MEDIUM**
- Code duplication increases maintenance burden
- Still complex due to path extraction

---

### Option 4: Config Preprocessing at Run() Entry Point

**Approach**: At workload start, preprocess operations and set global/per-agent multi-endpoint mode.

**Concept**:
```rust
// At start of run():
let effective_multi_ep_mode = determine_multi_endpoint_mode(&config);

// Then in operation execution:
// Don't check use_multi_endpoint flag, just use effective mode
```

**Pros**:
- ✅ No per-operation overhead
- ✅ Simpler execution path

**Cons**:
- ❌ **FATAL**: Loses per-operation granularity
- ❌ Can't mix multi-endpoint and single-endpoint operations in same workload
- ❌ Defeats the purpose of per-operation `use_multi_endpoint` flag
- ❌ Prepare vs workload can't have different endpoint strategies

**Risk Assessment**: **UNACCEPTABLE**
- Violates fundamental requirement for per-operation control

---

## Critical Unsolved Problem: URI Path Extraction

**All options require solving this**:

Given:
- URI: `file:///tmp/ep1/testdata/obj_123`
- Multi-endpoint config: `[file:///tmp/ep1/, file:///tmp/ep2/, file:///tmp/ep3/, file:///tmp/ep4/]`

Extract: `testdata/obj_123`

**Challenge**: The URI could have been constructed in multiple ways:
1. From pattern resolution (prepare/workload with base_uri)
2. From directory tree selector
3. From explicit full URIs in config

**Solution Approaches**:

**A. String Prefix Matching**:
```rust
fn extract_path_from_multi_endpoint_uri(
    uri: &str,
    endpoints: &[String]
) -> Result<String> {
    for endpoint in endpoints {
        if uri.starts_with(endpoint) {
            return Ok(uri[endpoint.len()..].to_string());
        }
    }
    Err(anyhow!("URI {} doesn't match any endpoint prefix", uri))
}
```

**Pros**: Simple, works for file:// URIs  
**Cons**: Fragile with trailing slashes, doesn't handle all URI schemes

**B. URI Parsing**:
```rust
fn extract_path_component(uri: &str, endpoints: &[String]) -> Result<String> {
    // Parse URI to components
    // Match against endpoint roots
    // Return relative path
    
    // For file:///tmp/ep1/testdata/obj_123:
    //   - Scheme: file://
    //   - Root: /tmp/ep1
    //   - Path: /testdata/obj_123
}
```

**Pros**: Robust, handles URL encoding, trailing slashes  
**Cons**: More complex, needs careful URI parsing

---

## Recommended Testing Strategy

**After implementing routing** (whichever option chosen):

1. **Unit Test**: Path extraction logic with various URI formats
2. **Integration Test**: Run with `--dry-run` to verify routing decisions
3. **Functional Test**: Run actual benchmark and verify:
   - Per-endpoint statistics show non-zero values
   - Load distribution matches strategy (round-robin = ~25% each)
   - Total requests = sum of endpoint requests

**Test Config**:
```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - file:///tmp/ep1/
    - file:///tmp/ep2/
    - file:///tmp/ep3/
    - file:///tmp/ep4/

workload:
  - op: get
    path: "file:///tmp/ep1/testdata/*"
    use_multi_endpoint: true  # Should route to MultiEndpointStore
    weight: 100
```

**Expected Output**:
```
Per-Endpoint Statistics:
  Endpoint 1: file:///tmp/ep1/
    Total requests: 25000
  Endpoint 2: file:///tmp/ep2/
    Total requests: 25000
  Endpoint 3: file:///tmp/ep3/
    Total requests: 25000
  Endpoint 4: file:///tmp/ep4/
    Total requests: 25000
  
  Load Distribution:
    Endpoint 1: 25.0% requests
    Endpoint 2: 25.0% requests
    Endpoint 3: 25.0% requests
    Endpoint 4: 25.0% requests
```

---

## Decision Framework

**Questions to Answer**:

1. **Architectural Purity vs Pragmatism**: 
   - Is Option 1's parameter threading worth the boilerplate?
   - Or is there a cleaner abstraction we're missing?

2. **Maintenance Burden**:
   - How often will we add new operation types?
   - How often will we modify cached operation signatures?

3. **Performance**:
   - Does per-operation flag checking add measurable overhead?
   - (Likely negligible - single bool check)

4. **Future Extensibility**:
   - Will we need other per-operation flags?
   - Should we introduce OperationContext now?

5. **Error Handling**:
   - What if `use_multi_endpoint=true` but no multi_endpoint config?
   - Config validation at load time vs runtime errors?

---

## My Concerns (Why I Hesitated)

1. **Felt like I was "simplifying" rather than solving correctly**
   - Path extraction is non-trivial
   - URI manipulation is error-prone
   - Need robust solution, not quick hack

2. **Parameter threading feels mechanical but correct**
   - It's the "obvious" solution
   - But maybe there's a better architecture?

3. **Worried about edge cases**:
   - What if endpoint has trailing slash and URI doesn't?
   - What if someone uses relative paths?
   - What about URI encoding in object names?

4. **Testing burden**:
   - Need comprehensive tests for path extraction
   - Need tests for all operation types × multi-endpoint
   - Integration testing with real backends

---

## Recommendation for Next Steps

1. **High-powered agent reviews this document**
2. **Makes architectural decision** (Option 1, 2, 3, 4, or something new)
3. **I implement the chosen approach** with confidence it's correct
4. **Comprehensive testing** before declaring complete

**Key Principle**: We're building production-quality tools. Get the architecture right, even if it takes longer.

---

## Current Git State

**Branch**: `feature/multi-endpoint-support`

**Uncommitted Changes**:
- ✅ Statistics infrastructure (ready to commit)
- ❌ Config field additions (ready to commit)
- ❌ Routing logic (pending architectural decision)

**Recommendation**: Commit statistics infrastructure and config changes separately before implementing routing. This keeps commits focused and reviewable.
