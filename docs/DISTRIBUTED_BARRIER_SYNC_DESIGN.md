# Distributed Barrier Synchronization Design

**Status**: Design Document  
**Created**: February 4, 2026  
**Version**: v0.8.24+  

## Problem Statement

Currently, agents in distributed mode work semi-independently with minimal coordination:
- Pre-flight validation runs sequentially on all agents (barrier exists)
- Workload execution starts independently (no barrier - agents can be in different phases)
- No synchronization between **prepare → execution → cleanup** phases
- Result: Agents can get "out of sync" - some preparing while others executing

**User Request**: Add barrier synchronization between phases while handling failures gracefully.

---

## Current Phase Flow

### Controller Orchestration
```
1. Connect to all agents (parallel)
   └─ FAIL if ANY agent unreachable
   
2. Pre-flight validation (sequential, one at a time)
   └─ BARRIER: All agents must pass before proceeding
   └─ FAIL if ANY agent has errors
   
3. Spawn workload streams (parallel, no coordination)
   ├─ Agent 1: prepare → execute → cleanup
   ├─ Agent 2: prepare → execute → cleanup (may finish prepare first)
   ├─ Agent 3: prepare → execute → cleanup (may still be preparing)
   └─ Agent 4: prepare → execute → cleanup (may start executing early)
   
   ❌ NO BARRIER between phases
   ❌ Agents can be in different phases simultaneously
```

### Agent State Machine
```rust
enum WorkloadState {
    Idle,      // Ready to accept new workload
    Ready,     // Config validated, waiting for start time
    Running,   // Workload executing (includes prepare phase)
    Aborting,  // Cleanup in progress
    Failed,    // Error occurred
}
```

**Issue**: `Running` encompasses **both** prepare and execute phases - no granularity!

---

## Proposed Solution: Multi-Phase Barriers

### Design Principles

1. **Barrier Synchronization**: All agents reach phase boundary before any proceed
2. **Timeout Handling**: Don't wait forever - fail fast on slow/dead agents
3. **Partial Success**: Some phases can proceed with reduced agent count (configurable)
4. **Error Recovery**: Graceful degradation vs hard failure (phase-dependent)

### New Agent State Machine

```rust
enum WorkloadState {
    Idle,              // Ready to accept new workload
    Validating,        // Pre-flight checks running
    PrepareReady,      // Validation passed, waiting for prepare barrier
    Preparing,         // Creating/cleaning objects
    ExecuteReady,      // Prepare complete, waiting for execute barrier
    Executing,         // Running I/O workload
    CleanupReady,      // Execution complete, waiting for cleanup barrier
    Cleaning,          // Removing objects
    Completed,         // All phases done successfully
    Failed(String),    // Error occurred (with reason)
    Aborting,          // Emergency shutdown in progress
}
```

### Barrier Types

```rust
#[derive(Debug, Clone, Copy)]
enum BarrierType {
    /// All agents must reach barrier (hard requirement)
    /// Missing agents cause entire workload to abort
    AllOrNothing,
    
    /// Majority (>50%) must reach barrier
    /// Stragglers are marked failed and excluded from next phase
    Majority,
    
    /// Best effort - proceed when liveness check fails on stragglers
    /// Stragglers continue independently (out of sync OK)
    BestEffort,
}

#[derive(Debug, Clone)]
struct BarrierConfig {
    barrier_type: BarrierType,
    
    // HEARTBEAT-BASED LIVENESS (not fixed timeouts!)
    heartbeat_interval_secs: u64,     // How often agents report (default: 30s)
    missed_heartbeats_threshold: u32, // How many misses before query (default: 3 = 90s)
    query_timeout_secs: u64,          // Timeout for explicit agent query (default: 10s)
    query_retry_count: u32,           // Retries for agent query (default: 2)
}
```

### Phase-Specific Barrier Configuration

```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # Default heartbeat settings (apply to all phases unless overridden)
    default_heartbeat_interval: 30s   # Agents report every 30s
    default_missed_threshold: 3       # 3 missed = 90s before query
    default_query_timeout: 10s        # Wait 10s for agent query response
    default_query_retries: 2          # Retry query 2 times before declaring dead
    
    # Pre-flight validation (CRITICAL - must have all agents)
    validation:
      type: all_or_nothing
      heartbeat_interval: 5s      # Faster checks during validation
      missed_threshold: 2         # 10s = dead during validation
    
    # Prepare phase (IMPORTANT - want all data created, but can be HOURS long)
    prepare:
      type: majority              # Can proceed with >50% if some agents fail
      heartbeat_interval: 30s     # Report every 30s
      missed_threshold: 3         # 90s without update before query
      query_timeout: 10s          # 10s to respond to query
      query_retries: 2            # Try 3 times total (1 + 2 retries)
      
      # Phase-specific progress reporting
      progress_fields:
        - objects_created         # How many objects created so far
        - objects_total          # Total objects to create
        - current_operation      # "creating", "listing", "validating", etc.
        - bytes_written          # Total bytes written
        - errors_encountered     # Error count
    
    # Execute phase (FLEXIBLE - can run for DAYS)
    execute:
      type: best_effort           # Proceed if agent dies during execution
      heartbeat_interval: 30s     # Still report every 30s
      missed_threshold: 3         # 90s grace period
      
      progress_fields:
        - operations_completed    # Total ops completed
        - current_throughput      # Ops/sec or GB/sec
        - phase_elapsed_time      # How long in this phase
    
    # Cleanup phase (BEST EFFORT - not critical, but can be LONG)
    cleanup:
      type: best_effort
      heartbeat_interval: 30s
      missed_threshold: 3
      
      progress_fields:
        - objects_deleted
        - objects_remaining
```

---

## Implementation Plan

### Phase 1: Extend Agent State Machine (1 day)

**File**: `src/bin/agent.rs`

1. Add new states to `WorkloadState` enum
2. Add state transition validation with detailed error messages
3. Add phase reporting in LiveStats:
   ```rust
   message LiveStats {
       // ... existing fields ...
       WorkloadPhase current_phase = 20;  // NEW
       uint64 phase_start_time_ms = 21;   // NEW
   }
   
   enum WorkloadPhase {
       PHASE_IDLE = 0;
       PHASE_VALIDATING = 1;
       PHASE_PREPARE_READY = 2;
       PHASE_PREPARING = 3;
       PHASE_EXECUTE_READY = 4;
       PHASE_EXECUTING = 5;
       PHASE_CLEANUP_READY = 6;
       PHASE_CLEANING = 7;
       PHASE_COMPLETED = 8;
       PHASE_FAILED = 9;
   }
   ```

### Phase 2: Add Barrier Coordination RPC (1 day)

**File**: `proto/iobench.proto`

```protobuf
// Heartbeat-based progress reporting (v0.8.25+)
message PhaseProgress {
    string agent_id = 1;
    WorkloadPhase current_phase = 2;
    uint64 phase_start_time_ms = 3;
    uint64 heartbeat_time_ms = 4;
    
    // Phase-specific progress fields
    uint64 objects_created = 10;      // Prepare phase
    uint64 objects_total = 11;        // Prepare phase
    string current_operation = 12;    // "creating", "listing", "cleaning", etc.
    uint64 bytes_written = 13;        // Prepare phase
    uint64 errors_encountered = 14;   // Any phase
    
    uint64 operations_completed = 20; // Execute phase
    double current_throughput = 21;   // Execute phase (ops/sec or GB/sec)
    uint64 phase_elapsed_ms = 22;     // Any phase
    
    uint64 objects_deleted = 30;      // Cleanup phase
    uint64 objects_remaining = 31;    // Cleanup phase
    
    // Liveness indicators
    bool is_stuck = 40;               // Agent reports it's stuck (deadlock, etc.)
    string stuck_reason = 41;         // Why agent thinks it's stuck
    double progress_rate = 42;        // Objects/sec or ops/sec (0 = not progressing)
}

// Barrier synchronization request
message BarrierRequest {
    string barrier_id = 1;     // "prepare", "execute", "cleanup"
    string agent_id = 2;
    PhaseProgress progress = 3; // Current progress (embedded heartbeat)
}

message BarrierResponse {
    bool proceed = 1;          // true = barrier satisfied, proceed to next phase
    repeated string waiting_agents = 2;  // Agents not yet at barrier
    repeated string failed_agents = 3;   // Agents that won't reach barrier
    uint64 next_heartbeat_ms = 4;        // When to send next heartbeat
}

// Explicit agent query (when heartbeats missed)
message AgentQueryRequest {
    string agent_id = 1;
    string reason = 2;  // "missed_heartbeats", "status_check", etc.
}

message AgentQueryResponse {
    PhaseProgress current_progress = 1;
    bool is_alive = 2;
    string status_message = 3;  // Human-readable status
}

service Agent {
    // ... existing RPCs ...
    
    // Agent reports readiness for next phase + sends heartbeat
    rpc ReportBarrierReady(BarrierRequest) returns (BarrierResponse);
    
    // Controller queries agent when heartbeats missed
    rpc QueryAgentStatus(AgentQueryRequest) returns (AgentQueryResponse);
}
```

### Phase 3: Controller Barrier Manager (2 days)

**File**: `src/bin/controller.rs`

```rust
struct AgentHeartbeat {
    agent_id: String,
    last_heartbeat: Instant,
    last_progress: PhaseProgress,
    missed_count: u32,
    is_alive: bool,
    query_in_progress: bool,
}

struct BarrierManager {
    agents: HashMap<String, AgentHeartbeat>,
    config: BarrierConfig,
    barrier_start: Instant,
    ready_agents: HashSet<String>,
    failed_agents: HashSet<String>,
}

impl BarrierManager {
    /// Process heartbeat from agent
    fn process_heartbeat(&mut self, agent_id: &str, progress: PhaseProgress) {
        if let Some(hb) = self.agents.get_mut(agent_id) {
            hb.last_heartbeat = Instant::now();
            hb.last_progress = progress;
            hb.missed_count = 0;  // Reset missed counter
            hb.is_alive = true;
            
            // Check if agent reached barrier (phase completed)
            if progress.phase_completed || progress.at_barrier {
                self.ready_agents.insert(agent_id.to_string());
            }
        }
    }
    
    /// Check for agents with missed heartbeats
    async fn check_liveness(&mut self, client_pool: &HashMap<String, AgentClient>) {
        let now = Instant::now();
        let heartbeat_interval = Duration::from_secs(self.config.heartbeat_interval_secs);
        
        for (agent_id, hb) in self.agents.iter_mut() {
            // Skip agents already marked as failed
            if self.failed_agents.contains(agent_id) {
                continue;
            }
            
            // Skip if heartbeat is recent
            if now.duration_since(hb.last_heartbeat) < heartbeat_interval {
                continue;
            }
            
            // Increment missed count
            hb.missed_count += 1;
            
            // If threshold exceeded and no query in progress, query the agent
            if hb.missed_count >= self.config.missed_heartbeats_threshold && !hb.query_in_progress {
                warn!("Agent {} missed {} heartbeats ({}s), querying status...",
                      agent_id, hb.missed_count, 
                      now.duration_since(hb.last_heartbeat).as_secs());
                
                hb.query_in_progress = true;
                
                // Query agent with retries
                match self.query_agent_with_retry(agent_id, client_pool).await {
                    Ok(response) => {
                        info!("✅ Agent {} responded to query: {}", agent_id, response.status_message);
                        self.process_heartbeat(agent_id, response.current_progress);
                        hb.query_in_progress = false;
                    }
                    Err(e) => {
                        error!("❌ Agent {} failed to respond after {} retries: {}",
                               agent_id, self.config.query_retry_count, e);
                        hb.is_alive = false;
                        hb.query_in_progress = false;
                        self.failed_agents.insert(agent_id.to_string());
                    }
                }
            }
        }
    }
    
    /// Query agent with exponential backoff retry
    async fn query_agent_with_retry(
        &self,
        agent_id: &str,
        client_pool: &HashMap<String, AgentClient>,
    ) -> Result<AgentQueryResponse> {
        let client = client_pool.get(agent_id)
            .ok_or_else(|| anyhow!("No client for agent {}", agent_id))?;
        
        let mut retries = 0;
        let mut backoff_ms = 1000;  // Start with 1s
        
        loop {
            let request = AgentQueryRequest {
                agent_id: agent_id.to_string(),
                reason: "missed_heartbeats".to_string(),
            };
            
            match tokio::time::timeout(
                Duration::from_secs(self.config.query_timeout_secs),
                client.clone().query_agent_status(request)
            ).await {
                Ok(Ok(response)) => return Ok(response.into_inner()),
                Ok(Err(e)) => {
                    if retries >= self.config.query_retry_count {
                        return Err(anyhow!("Query failed after {} retries: {}", retries, e));
                    }
                    warn!("Query attempt {} failed for {}: {}, retrying in {}ms",
                          retries + 1, agent_id, e, backoff_ms);
                }
                Err(_) => {
                    if retries >= self.config.query_retry_count {
                        return Err(anyhow!("Query timeout after {} retries", retries));
                    }
                    warn!("Query attempt {} timeout for {}, retrying in {}ms",
                          retries + 1, agent_id, backoff_ms);
                }
            }
            
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            retries += 1;
            backoff_ms = (backoff_ms * 2).min(10_000);  // Cap at 10s
        }
    }
    
    /// Check if barrier is satisfied (all/majority/best-effort)
    fn check_barrier(&self) -> BarrierStatus {
        let ready_count = self.ready_agents.len();
        let total_count = self.agents.len();
        let failed_count = self.failed_agents.len();
        let alive_count = total_count - failed_count;
        
        match self.config.barrier_type {
            BarrierType::AllOrNothing => {
                if ready_count == alive_count {
                    BarrierStatus::Ready
                } else if failed_count > 0 {
                    // Any failure in AllOrNothing mode = abort
                    BarrierStatus::Failed
                } else {
                    BarrierStatus::Waiting
                }
            }
            
            BarrierType::Majority => {
                if ready_count > alive_count / 2 {
                    BarrierStatus::Ready
                } else if alive_count - ready_count < alive_count / 2 {
                    // Not enough agents left to reach majority
                    BarrierStatus::Failed
                } else {
                    BarrierStatus::Waiting
                }
            }
            
            BarrierType::BestEffort => {
                if ready_count == alive_count {
                    BarrierStatus::Ready
                } else if ready_count > 0 && failed_count > 0 {
                    // Some ready, some failed - proceed with ready ones
                    BarrierStatus::Degraded
                } else {
                    BarrierStatus::Waiting
                }
            }
        }
    }
    
    /// Wait for barrier with heartbeat-based liveness checking
    async fn wait_for_barrier(
        &mut self, 
        phase_name: &str,
        client_pool: &HashMap<String, AgentClient>,
    ) -> Result<BarrierStatus> {
        let start = Instant::now();
        let mut last_liveness_check = Instant::now();
        let liveness_check_interval = Duration::from_secs(
            self.config.heartbeat_interval_secs.max(10)
        );
        
        loop {
            // Check barrier status
            let status = self.check_barrier();
            
            match status {
                BarrierStatus::Ready => {
                    info!("✅ Barrier '{}' ready: all {} agents synchronized (elapsed: {:?})", 
                          phase_name, self.ready_agents.len(), start.elapsed());
                    return Ok(status);
                }
                
                BarrierStatus::Degraded => {
                    warn!("⚠️  Barrier '{}' degraded: {}/{} agents ready, {} failed (elapsed: {:?})", 
                          phase_name, 
                          self.ready_agents.len(), 
                          self.agents.len(),
                          self.failed_agents.len(),
                          start.elapsed());
                    return Ok(status);
                }
                
                BarrierStatus::Failed => {
                    error!("❌ Barrier '{}' failed: only {}/{} agents ready, {} failed",
                           phase_name, self.ready_agents.len(), self.agents.len(), self.failed_agents.len());
                    return Err(anyhow!("Barrier failed - insufficient agents"));
                }
                
                BarrierStatus::Waiting => {
                    // Check liveness periodically
                    if last_liveness_check.elapsed() >= liveness_check_interval {
                        self.check_liveness(client_pool).await;
                        last_liveness_check = Instant::now();
                    }
                    
                    // Poll every 100ms
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    
                    // Log progress every 10 seconds
                    if start.elapsed().as_secs() % 10 == 0 && start.elapsed().as_millis() % 10000 < 100 {
                        let waiting: Vec<_> = self.agents.keys()
                            .filter(|a| !self.ready_agents.contains(*a) && !self.failed_agents.contains(*a))
                            .map(|s| {
                                let hb = &self.agents[s];
                                format!("{} ({}s ago, {} ops)", 
                                    s, 
                                    Instant::now().duration_since(hb.last_heartbeat).as_secs(),
                                    hb.last_progress.operations_completed)
                            })
                            .collect();
                        
                        info!("Barrier '{}': {}/{} ready, waiting for: {}",
                               phase_name,
                               self.ready_agents.len(),
                               self.agents.len() - self.failed_agents.len(),
                               waiting.join(", "));
                    }
                }
            }
        }
    }
}

enum BarrierStatus {
    Ready,     // All required agents at barrier
    Waiting,   // Still waiting for agents
    Degraded,  // Some failed, proceeding with survivors
    Failed,    // Insufficient agents ready
}
```

### Phase 4: Integration into Workload Execution (2 days)

**Modify**: `run_distributed_workload()`

```rust
async fn run_distributed_workload(...) -> Result<()> {
    // ... existing pre-flight validation ...
    
    // ===================================================================
    // PHASE 1: PREPARE BARRIER
    // ===================================================================
    println!("\n📋 Phase 1: Prepare (creating {} objects)", total_object_count);
    
    let mut prepare_barrier = BarrierManager::new(
        ids.clone(),
        config.distributed.barrier_sync.prepare.clone(),
    );
    
    // Send prepare command to all agents
    for (agent_id, client) in &mut agent_clients {
        client.start_prepare_phase(...).await?;
    }
    
    // Wait for all agents to complete prepare OR timeout
    match prepare_barrier.wait_for_barrier("prepare").await? {
        BarrierStatus::Ready => {
            println!("✅ All agents ready for execution");
        }
        BarrierStatus::Degraded => {
            println!("⚠️  Proceeding with {}/{} agents (some timed out)",
                     prepare_barrier.ready_agents.len(),
                     prepare_barrier.agents.len());
            // Update active agent list (exclude failed agents)
            agent_clients.retain(|(id, _)| !prepare_barrier.failed_agents.contains(id));
        }
        _ => unreachable!(),  // wait_for_barrier returns error for timeout/failed
    }
    
    // ===================================================================
    // PHASE 2: EXECUTE BARRIER
    // ===================================================================
    println!("\n🏃 Phase 2: Execute (duration: {}s)", config.duration.as_secs());
    
    let mut execute_barrier = BarrierManager::new(
        agent_clients.iter().map(|(id, _)| id.clone()).collect(),
        config.distributed.barrier_sync.execute.clone(),
    );
    
    // Send execute command
    for (agent_id, client) in &mut agent_clients {
        client.start_execute_phase(...).await?;
    }
    
    // Wait for barrier (likely best_effort - proceed after timeout)
    execute_barrier.wait_for_barrier("execute").await?;
    
    // ... monitor workload execution ...
    
    // ===================================================================
    // PHASE 3: CLEANUP BARRIER
    // ===================================================================
    println!("\n🧹 Phase 3: Cleanup (removing {} objects)", total_object_count);
    
    let mut cleanup_barrier = BarrierManager::new(
        agent_clients.iter().map(|(id, _)| id.clone()).collect(),
        config.distributed.barrier_sync.cleanup.clone(),
    );
    
    // Send cleanup command
    for (agent_id, client) in &mut agent_clients {
        client.start_cleanup_phase(...).await?;
    }
    
    cleanup_barrier.wait_for_barrier("cleanup").await?;
    
    println!("✅ All phases complete");
}
```

---

## Error Recovery Strategies

### Scenario 1: Agent Dies During Prepare
- **Detection**: 3 consecutive heartbeats missed (90s default)
- **Action**:
  1. Controller sends `QueryAgentStatus` RPC
  2. Retry with exponential backoff (1s, 2s, 4s)
  3. After 3 failed queries → mark agent as `failed`
  4. Check barrier type:
     - **AllOrNothing**: Abort entire workload, cleanup partial data
     - **Majority**: Continue if >50% still alive
     - **BestEffort**: Continue with remaining agents
  5. Log failure prominently in console + results
  6. Exclude failed agent from subsequent phases

### Scenario 2: Agent Slow During Prepare (Not Dead, Just Slow)
- **Detection**: Heartbeats still arriving, but progress_rate near zero
- **Action**:
  1. Agent reports `is_stuck: false` with valid progress (e.g., "created 1M/10M objects")
  2. Controller logs: "Agent X preparing slowly: 100K ops/sec (expected 500K)"
  3. **Do NOT mark as failed** - agent is making progress
  4. Wait indefinitely (no timeout!) as long as heartbeats arrive
  5. Barrier satisfied when agent reports completion, regardless of duration
  6. Display per-agent progress in console (see below)

**Key Insight**: Slow is OK, stuck/dead is not. Heartbeats differentiate.

### Scenario 3: Agent Reports Stuck (Deadlock/Infinite Loop)
- **Detection**: Agent sends `is_stuck: true` with reason
- **Action**:
  1. Log error: "Agent X reports stuck: {stuck_reason}"
  2. Mark agent as failed (self-reported failure)
  3. Proceed based on barrier type (same as Scenario 1)
  4. Optionally: Send abort signal to stuck agent

### Scenario 4: Network Partition (Agent Unreachable But Alive)
- **Detection**: Heartbeats stop, `QueryAgentStatus` timeout
- **Action**:
  1. Retry query with exponential backoff (3 attempts over ~7s)
  2. If reconnect succeeds during retry:
     - Process queued heartbeats
     - Verify agent still in same phase
     - Continue normally
  3. If all retries fail → treat as dead agent (Scenario 1)
  4. If agent reconnects later:
     - Log warning: "Agent X recovered after {}s offline"
     - If still in same phase → rejoin barrier
     - If ahead (shouldn't happen) → error and abort that agent

### Scenario 5: Controller Restart (All Agents Still Running)
- **Current Behavior**: Agents continue independently, no coordination
- **With Heartbeats**: Agents detect controller disconnect
- **Solution**:
  1. Agents continue work but buffer heartbeats in memory (last 10)
  2. Controller restart queries all agents for status
  3. Rebuild barrier state from agent responses:
     ```rust
     // Query all agents for current state
     for agent in agents {
         let response = query_agent_status(agent).await?;
         barrier_manager.process_heartbeat(agent, response.current_progress);
     }
     // Barrier manager now has current state, continue from there
     ```
  4. Resume coordination from current phase

---

## Configuration Examples

### Strict Mode (Research/Benchmarking - Need Exact Coordination)
```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # Fast heartbeats for tight coordination
    default_heartbeat_interval: 10s
    default_missed_threshold: 3      # 30s grace period
    default_query_timeout: 5s
    default_query_retries: 2
    
    validation:
      type: all_or_nothing
      heartbeat_interval: 5s         # Very fast during validation
      missed_threshold: 2            # 10s = dead
    
    prepare:
      type: all_or_nothing           # MUST have all agents
      heartbeat_interval: 30s        # Regular updates even for hours-long prepare
      missed_threshold: 3            # 90s grace before query
      query_retries: 2               # Try hard to reach agent
    
    execute:
      type: all_or_nothing           # Perfect synchronization
      heartbeat_interval: 10s        # Frequent updates during execution
      missed_threshold: 3
    
    cleanup:
      type: all_or_nothing           # Clean environment
      heartbeat_interval: 30s
      missed_threshold: 3
```

### Relaxed Mode (Production Testing - Tolerance for Failures)
```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # Standard heartbeats, lenient thresholds
    default_heartbeat_interval: 30s
    default_missed_threshold: 4      # 120s before query
    default_query_timeout: 10s
    default_query_retries: 3         # Extra retries for flaky networks
    
    validation:
      type: all_or_nothing           # Still want all agents initially
      heartbeat_interval: 5s
      missed_threshold: 2
    
    prepare:
      type: majority                 # Can lose some agents
      heartbeat_interval: 30s        # Works for multi-hour prepares
      missed_threshold: 4            # 120s grace (lenient)
      query_retries: 3
    
    execute:
      type: best_effort              # Just run what we have
      heartbeat_interval: 30s
      missed_threshold: 4
    
    cleanup:
      type: best_effort              # Best effort cleanup
      heartbeat_interval: 60s        # Slower heartbeats OK
      missed_threshold: 3            # 180s grace
```

### Long-Running Mode (Multi-Day Workloads)
```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # Longer intervals for efficiency, but still responsive
    default_heartbeat_interval: 60s  # 1 minute heartbeats
    default_missed_threshold: 5      # 5 minutes before query
    default_query_timeout: 30s       # Longer query timeout
    default_query_retries: 5         # Very patient retries
    
    validation:
      type: all_or_nothing
      heartbeat_interval: 10s        # Faster for initial validation
      missed_threshold: 3
    
    prepare:
      type: majority
      heartbeat_interval: 60s        # 1 min updates even for 48-hour prepare
      missed_threshold: 5            # 5 min grace period
      query_timeout: 30s
      query_retries: 5               # Patient with slow networks
    
    execute:
      type: best_effort
      heartbeat_interval: 120s       # 2 min updates (workload may run for days)
      missed_threshold: 3            # 6 min grace
      query_retries: 5
    
    cleanup:
      type: best_effort
      heartbeat_interval: 60s
      missed_threshold: 5
```

### Current Behavior (No Barriers - Legacy)
```yaml
distributed:
  barrier_sync:
    enabled: false  # DEFAULT for backward compatibility
```
      type: all_or_nothing
      timeout: 30s
    prepare:
      type: all_or_nothing  # MUST have all agents
      timeout: 600s
      retry_count: 2
    execute:
      type: all_or_nothing  # Perfect synchronization
      timeout: 60s
      retry_count: 0
    cleanup:
      type: all_or_nothing  # Clean environment
      timeout: 300s
      retry_count: 1
```

### Relaxed Mode (Production Testing - Tolerance for Failures)
```yaml
distributed:
  barrier_sync:
    enabled: true
    validation:
      type: all_or_nothing  # Still want all agents initially
      timeout: 30s
    prepare:
      type: majority  # Can lose some agents
      timeout: 300s
      retry_count: 1
    execute:
      type: best_effort  # Just run what we have
      timeout: 30s
      retry_count: 0
    cleanup:
      type: best_effort  # Best effort cleanup
      timeout: 60s
      retry_count: 0
```

### Current Behavior (No Barriers - Legacy)
```yaml
distributed:
  barrier_sync:
    enabled: false  # DEFAULT for backward compatibility
```

---

## Console Display During Barrier Wait

### Real-Time Progress Table (Updated Every 2 Seconds)

```
📋 Phase: Prepare (Barrier: Majority)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Agent      Status      Progress         Rate         Last HB    State
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
agent-1    ✅ Ready    16M/16M objects  245K ops/s   2s ago     Waiting at barrier
agent-2    🏃 Working  12M/16M objects  198K ops/s   1s ago     Creating objects
agent-3    🏃 Working  14M/16M objects  210K ops/s   3s ago     Creating objects
agent-4    ⚠️  Query   8M/16M objects   92K ops/s    94s ago    Querying (attempt 2/3)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Barrier: 1/4 ready (need 3/4 for majority) | Elapsed: 18m 42s | ETA: ~4m (based on slowest)
```

**Legend**:
- ✅ **Ready**: Agent at barrier, waiting for others
- 🏃 **Working**: Agent actively making progress (heartbeats arriving)
- ⚠️ **Query**: Missed heartbeats, controller querying agent
- ❌ **Failed**: Agent dead/unreachable, excluded from barrier
- 💤 **Slow**: Making progress but below expected rate

### Detailed Agent Status (On Demand via Hotkey)

Press `d` for detailed agent status:
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Agent: agent-4 (⚠️  Querying)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Phase: Preparing
  Progress: 8,234,567 / 16,000,000 objects (51.5%)
  Rate: 92,145 ops/sec (expected: 200,000 ops/sec)
  Bytes Written: 130.2 GB / 252.4 GB
  
  Last Heartbeat: 94 seconds ago
  Missed Heartbeats: 3 (threshold: 3)
  Query Status: In progress (attempt 2/3)
  Query Timeout: 8 seconds remaining
  
  Current Operation: "creating objects (batch 8234-8235)"
  Errors: 0
  
  Timeline:
    18:42:10  Phase started
    18:58:23  Last heartbeat received
    19:00:57  Missed heartbeat #1 (expected 18:58:53)
    19:01:27  Missed heartbeat #2 (expected 19:00:23)
    19:01:57  Missed heartbeat #3 (expected 19:00:53)
    19:02:00  Query initiated (attempt 1) - timeout
    19:02:15  Query initiated (attempt 2) - in progress
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

---

## Performance Considerations

### Overhead (Heartbeat-Based Approach)

**Network Traffic**:
- **Per Agent**: 1 heartbeat every 30s = ~500 bytes/heartbeat
- **4 Agents**: 4 × 500 bytes = 2 KB every 30s = ~67 bytes/sec
- **100 Agents**: 100 × 500 bytes = 50 KB every 30s = ~1.7 KB/sec
- **Negligible**: Even with 1000 agents, <20 KB/sec total

**CPU/Memory**:
- **Heartbeat Processing**: ~10μs per heartbeat (HashMap lookup + timestamp update)
- **Liveness Check**: ~100μs per agent (iterate HashMap, check timestamps)
- **Per-second overhead**: <1ms CPU even with 100 agents

**Comparison to Fixed Timeout Approach**:
- ✅ **Better**: No artificial phase timeouts (works for any duration)
- ✅ **Better**: Faster failure detection (90s vs 5 min timeout)
- ✅ **Better**: Real-time progress visibility (know WHY agent is slow)
- ✅ **Same**: Network overhead (heartbeats already exist in LiveStats)
- ✅ **Same**: Barrier coordination logic (still need to check all/majority/best-effort)

### Scalability
- **Small Scale (1-10 agents)**: Negligible impact (<0.1% overhead)
- **Medium Scale (10-100 agents)**: Still negligible (<1% overhead)
- **Large Scale (100-1000 agents)**: ~1-2% overhead from heartbeat processing
  - **Optimization**: Batch heartbeat processing (process in 100ms windows)
  - **Optimization**: Hierarchical coordination (regional controllers)

### Heartbeat Interval Tuning

| Workload Duration | Recommended Interval | Missed Threshold | Liveness Latency |
|-------------------|---------------------|------------------|------------------|
| <1 minute | 5s | 2 | 10s |
| 1-10 minutes | 10s | 3 | 30s |
| 10-60 minutes | 30s | 3 | 90s |
| 1-24 hours | 60s | 3 | 3 minutes |
| >24 hours | 120s | 5 | 10 minutes |

**Rule of Thumb**: Heartbeat interval should be ~1-2% of expected phase duration, with 90s-5min liveness latency acceptable for most workloads.

---
- **Medium Scale (10-100 agents)**: Linear scaling, ~10-50ms per agent
- **Large Scale (100+ agents)**: May need hierarchical barriers (v2 feature)

---

## Testing Strategy

### Unit Tests
1. `BarrierManager::check_barrier()` logic for all types
2. Timeout calculation and retry logic
3. State transition validation

### Integration Tests
1. 4-agent test with all agents reaching barriers
2. 4-agent test with 1 slow agent (timeout scenarios)
3. 4-agent test with 1 dead agent (failure scenarios)
4. Network partition simulation (reconnect logic)

### Manual Testing
1. **4-host remote test** with barriers enabled
2. Kill one agent during prepare → verify majority proceeds
3. Slow network simulation (tc/netem) → verify timeouts work
4. Controller restart during execution → verify recovery

---

## Rollout Plan

### v0.8.25: Foundation (1 week)
- [ ] Extend agent state machine
- [ ] Add phase reporting in LiveStats
- [ ] Update proto definitions
- [ ] Basic BarrierManager implementation
- [ ] Default: `barrier_sync.enabled: false` (backward compatible)

### v0.8.26: Controller Integration (1 week)
- [ ] Integrate barriers into `run_distributed_workload()`
- [ ] Add console display for barrier status
- [ ] Configuration parsing and validation
- [ ] Documentation and examples

### v0.8.27: Error Recovery (1 week)
- [ ] Agent failure detection and handling
- [ ] Network partition recovery
- [ ] Straggler detection and warnings
- [ ] Comprehensive testing on 4-host system

### v0.8.28: Advanced Features (optional)
- [ ] Hierarchical barriers for 100+ agents
- [ ] Dynamic barrier timeout adjustment
- [ ] Barrier skip for specific phases
- [ ] Performance profiling and optimization

---

## Open Questions

1. **Should barriers be required or optional?**
   - **Answer**: Optional via `barrier_sync.enabled: false` (default for v0.8.25)
   - **Rationale**: Backward compatibility, gradual rollout

2. **What happens if agent phases get out of sync?**
   - **Scenario**: Agent A at "Executing", Agent B still "Preparing"
   - **Current**: Both run independently (can happen now)
   - **With Barriers**: Agent A waits at execute barrier, Agent B catches up
   - **If B slow/stuck**: Based on barrier type (majority proceeds without B)
   - **Heartbeats show**: B is making progress (100K ops/sec) so keep waiting

3. **Should we allow agents to skip phases?**
   - **Example**: `skip_prepare: true` if data already exists
   - **Issue**: Barrier coordination becomes complex (some at prepare, some at execute)
   - **Answer**: No skipping with barriers enabled (validate all agents at same phase)

4. **How to handle controller failures?**
   - **Answer**: Combination approach:
     - Agents continue work, buffer heartbeats in memory (last 10)
     - Controller restart queries all agents, rebuilds barrier state
     - If controller doesn't restart, agents timeout after 2× heartbeat threshold
   - **Future**: Option 3 (persistent checkpointing) for long-running workloads

5. **Display strategy during barrier wait?**
   - **Answer**: Real-time progress table (Option 3) with:
     - Per-agent status (Ready/Working/Query/Failed)
     - Progress (12M/16M objects)
     - Rate (198K ops/sec)
     - Last heartbeat timestamp
     - Detailed view on demand (hotkey)

6. **How long to wait for slow agents?**
   - **Answer**: NO FIXED TIMEOUT - wait indefinitely as long as heartbeats arrive
   - **Key Insight**: Slow ≠ Dead. Heartbeats differentiate:
     - Slow: "Created 100K objects, rate 10K/sec" → keep waiting
     - Dead: No heartbeat for 90s → query, then mark failed
   - **This solves multi-hour/day workload problem!**

---

## Conclusion

**Heartbeat-based barrier synchronization solves BOTH problems**:
1. **Coordination**: Agents move through phases together
2. **Flexibility**: Works for any duration (minutes to days) via heartbeats

### Key Advantages Over Fixed Timeouts

| Aspect | Fixed Timeout | Heartbeat-Based | Winner |
|--------|---------------|-----------------|--------|
| Multi-day workloads | Fails (need huge timeout) | Works (no timeout!) | ✅ Heartbeat |
| Failure detection | Timeout only | Active query + retry | ✅ Heartbeat |
| Progress visibility | None | Real-time per agent | ✅ Heartbeat |
| Slow vs stuck | Can't tell | Clear distinction | ✅ Heartbeat |
| Configuration | Phase duration guess | Heartbeat interval | ✅ Heartbeat |
| Network overhead | Same | Same | 🟰 Tie |

### Benefits
1. **Visibility**: Console shows real-time progress for each agent
2. **Control**: Admin decides strictness per phase (all/majority/best-effort)
3. **Reliability**: Handles failures gracefully without hanging forever
4. **Performance**: <1% overhead even with 100 agents
5. **Scalability**: Works for workloads from 1 minute to 30 days
6. **Debugging**: Know exactly why barriers are waiting (agent X at 51%, 92K ops/sec)

### Configuration Simplicity
```yaml
# Simple: Just set heartbeat interval (not phase duration!)
barrier_sync:
  default_heartbeat_interval: 30s   # Same for 1-hour or 48-hour phases
  default_missed_threshold: 3       # 90s before query
```

**vs old approach**:
```yaml
# Complex: Need to guess phase duration for EACH phase
prepare:
  timeout: 300s   # Wrong for 2-hour dataset!
execute:
  timeout: 60s    # Wrong for 24-hour run!
```
### Implementation is tractable
- **Week 1-2**: Proto changes + agent state machine + heartbeat infrastructure
- **Week 3**: Controller barrier manager + liveness checking
- **Week 4**: Integration + testing on 4-host system

### Rollout Strategy
1. **v0.8.25**: Core infrastructure (`enabled: false` by default)
2. **v0.8.26**: User testing with `prepare` barrier only
3. **v0.8.27**: Full 3-phase barriers + error recovery
4. **v0.8.28**: Polish + hierarchical scaling

**Next Steps**: Discuss priority, start with prepare-only barrier in v0.8.25?

