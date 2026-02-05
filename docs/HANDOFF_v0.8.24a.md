# Error Recovery and Progress Reporting Improvements - v0.8.24a

**Status**: Implementation Handoff  
**Created**: February 4, 2026  
**Version**: v0.8.24a (immediate improvements before v0.8.25 barrier sync)  
**Priority**: CRITICAL - Production Stability  
**Estimated Effort**: 3-4 days

---

## Executive Summary

**CRITICAL PRODUCTION ISSUES DISCOVERED**:

1. **Silent TreeManifest Creation**: 331,776 directory tree takes 30-90s to generate with ZERO progress updates → controller thinks agents are dead → h2 protocol timeout
2. **Catastrophic Error Handling**: Controller dies → agents hang indefinitely in weird state (no retry, no recovery, no cleanup)
3. **Cryptic Error Messages**: "h2 protocol error xyz" tells user NOTHING about what failed or why
4. **Work Before Handshake**: Expensive operations (TreeManifest) start before gRPC stream confirms agents are ready
5. **No Liveness During Long Operations**: Prepare phase can run for HOURS with no progress updates (only final counts)

**USER FEEDBACK** (verbatim):
- "timeout happens BEFORE the prepare stage starts...with skip_verification=true AND force_overwrite=true set, and the timeout STILL happened"
- "error handling is VERY bad...agents just bail out and leave all the agents in a weird state...needs to be SIGNIFICANTLY improved"
- "It should go into a reset loop, or retry or something, instead it just dies and leaves the agents, literally hanging"
- "We REALLY need to fix this, WHY is this so hard? I do NOT understand why vdbench works PERFECTLY for this use case"

**ROOT CAUSE**: TreeManifest creation at [src/prepare.rs:728](src/prepare.rs#L728) is a **synchronous CPU-bound operation** that:
- Generates 331k directory path strings
- Builds HashMap with all paths
- Runs for 30-90 seconds
- Happens BEFORE prepare phase starts sending LiveStats
- Has ZERO progress reporting
- Blocks gRPC stream → controller timeout

---

## Scope: Pre-Barrier Improvements (v0.8.24a)

This handoff covers **IMMEDIATE STABILITY FIXES** before implementing full barrier synchronization (documented in DISTRIBUTED_BARRIER_SYNC_DESIGN.md).

### What This Includes

✅ **Progress Reporting**: LiveStats updates during ALL long operations  
✅ **Error Recovery**: Retry loops with exponential backoff instead of immediate exit  
✅ **Human-Readable Errors**: Clear messages explaining what failed and why  
✅ **Deferred Work**: Don't start expensive operations until handshake completes  
✅ **Agent Resilience**: Detect controller death and reset to Idle state  

### What This Defers to v0.8.25

⏳ **Barrier Synchronization**: Phase coordination between agents (3-4 weeks, see DISTRIBUTED_BARRIER_SYNC_DESIGN.md)  
⏳ **Heartbeat-Based Liveness**: 30s interval, 3 missed = query agent (part of barrier system)  
⏳ **Phase-Specific Config**: All/Majority/BestEffort barrier modes per phase  

**Rationale**: Fix immediate production stability issues NOW, then add full coordination system in v0.8.25.

---

## Problem 1: Silent TreeManifest Creation (CRITICAL)

### Current Behavior

**Code Location**: [src/prepare.rs:715-735](src/prepare.rs#L715-L735)

```rust
// v0.7.9: Create tree manifest FIRST if directory_structure is configured
// This way file creation can use proper directory paths
let tree_manifest = if config.directory_structure.is_some() {
    info!("Creating directory tree structure (agent {}/{})...", agent_id, num_agents);
    
    let base_uri = config.ensure_objects.first()
        .and_then(|spec| {
            // Get effective base_uri - use first endpoint if base_uri is None
            let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
            spec.get_base_uri(multi_endpoint_uris).ok()
        })
        .ok_or_else(|| anyhow!("directory_structure requires at least one ensure_objects entry for base_uri"))?;
    
    let manifest = create_tree_manifest_only(config, agent_id, num_agents, &base_uri)?;  // ❌ BLOCKS 30-90s
    
    if num_agents > 1 {
        info!("Tree structure: {} directories, {} files total ({} assigned to this agent)", 
            manifest.total_dirs, manifest.total_files, 
            manifest.get_agent_file_indices(agent_id, num_agents).len());
    }
    
    Some(manifest)
} else {
    None
};
```

**Problem**: `create_tree_manifest_only()` at line 728 runs synchronously and:
1. Generates 24^depth directory paths (331,776 for depth=4)
2. Creates HashMap entries for all paths
3. Calculates agent assignments
4. Takes 30-90 seconds for large trees
5. **ZERO progress updates sent to controller**
6. Happens **BEFORE** prepare phase starts (line 740+)
7. Happens **BEFORE** LiveStats tracker receives first update

**Impact**: Controller sees no activity for 60-90s → gRPC timeout → h2 protocol error

### Solution 1A: Incremental Progress Updates

**Send LiveStats updates every N directories during TreeManifest creation**

**Implementation Plan**:

1. **Pass LiveStats tracker to tree creation** (modify function signature)
   ```rust
   // src/prepare.rs:2039 - Update signature
   pub fn create_tree_manifest_only(
       config: &PrepareConfig,
       agent_id: usize,
       num_agents: usize,
       base_uri: &str,
       live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,  // NEW
   ) -> Result<TreeManifest>
   ```

2. **Add progress reporting in DirectoryTree::generate()** (src/directory_tree.rs)
   ```rust
   // src/directory_tree.rs:113 - In generate() function
   fn generate(&mut self, live_stats_tracker: Option<&LiveStatsTracker>) -> Result<()> {
       // Calculate total directories: width^1 + width^2 + ... + width^depth
       self.total_directories = 0;
       for level in 1..=self.config.depth {
           let dirs_at_level = self.config.width.pow(level as u32);
           self.total_directories += dirs_at_level;
       }
       
       let report_interval = 5000;  // Report every 5000 directories
       let mut dirs_processed = 0;
       
       // Generate all directory nodes
       for level in 1..=self.config.depth {
           let dirs_at_level = self.config.width.pow(level as u32);
           let mut level_paths = Vec::new();
           
           for width_idx in 1..=dirs_at_level {
               let node = self.create_node(level, width_idx)?;
               let key = format!("{}_{}", level, width_idx);
               level_paths.push(node.full_path.clone());
               self.all_paths.push(node.full_path.clone());
               self.directories.insert(key, node);
               
               dirs_processed += 1;
               
               // 🆕 Send progress update every N directories
               if dirs_processed % report_interval == 0 {
                   if let Some(tracker) = live_stats_tracker {
                       tracker.set_prepare_progress(
                           dirs_processed,
                           self.total_directories,
                           "generating tree structure".to_string(),
                       );
                   }
                   
                   // Also log to console for standalone mode
                   debug!("Tree generation: {}/{} directories ({:.1}%)", 
                          dirs_processed, self.total_directories,
                          (dirs_processed as f64 / self.total_directories as f64) * 100.0);
               }
           }
           
           self.by_level.insert(level, level_paths);
       }
       
       // Final progress update
       if let Some(tracker) = live_stats_tracker {
           tracker.set_prepare_progress(
               self.total_directories,
               self.total_directories,
               "tree structure complete".to_string(),
           );
       }
       
       // ... rest of function ...
   }
   ```

3. **Update caller in prepare.rs** (line 728)
   ```rust
   let manifest = create_tree_manifest_only(
       config, 
       agent_id, 
       num_agents, 
       &base_uri,
       live_stats_tracker.clone(),  // 🆕 Pass tracker
   )?;
   ```

**Expected Outcome**: Controller sees updates every 5000 directories (every ~1-2 seconds), no timeout.

### Solution 1B: Defer TreeManifest Until After Handshake

**Don't create manifest until gRPC stream is established and agent confirms ready**

**Implementation Plan**:

1. **Move TreeManifest creation after state transition** (src/bin/agent.rs)
   ```rust
   // Currently: manifest created in prepare_and_execute_workload() before state change
   // New: Create manifest AFTER transitioning to Running state
   
   // In execute_workload RPC handler (src/bin/agent.rs:1050+)
   async fn execute_workload(
       &self,
       request: Request<Streaming<WorkloadRequest>>,
   ) -> Result<Response<Self::ExecuteWorkloadStream>, Status> {
       // ... existing code ...
       
       // Transition to Running FIRST
       {
           let mut state = self.state.write().await;
           *state = WorkloadState::Running;
       }
       
       // 🆕 Send initial heartbeat BEFORE expensive operations
       let initial_stats = self.live_stats_aggregator.get_current_stats().await;
       tx.send(Ok(initial_stats)).await
           .map_err(|e| Status::internal(format!("Failed to send initial stats: {}", e)))?;
       
       // NOW create tree manifest (controller knows we're alive)
       let manifest = if config.directory_structure.is_some() {
           Some(create_tree_manifest_with_progress(...).await?)
       } else {
           None
       };
       
       // ... rest of execution ...
   }
   ```

**Expected Outcome**: Controller receives initial heartbeat within 1-2s, knows agents are alive, won't timeout during manifest creation.

### Recommended Approach

**Implement BOTH 1A and 1B**:
- 1A provides visibility into expensive operation (good UX)
- 1B ensures controller knows agents are responsive (prevents timeout)
- Combined: robust solution that works for trees up to 1M+ directories

---

## Problem 2: Catastrophic Error Handling (CRITICAL)

### Current Behavior

**When controller dies or encounters error**:
1. Controller process exits immediately
2. Agents continue executing workload
3. Agents try to send LiveStats → stream closed → panic/error
4. Agents sit in weird state (not Idle, not Running, not Failed)
5. NO retry logic
6. NO cleanup
7. NO graceful degradation

**User Quote**: "It should go into a reset loop, or retry or something, instead it just dies and leaves the agents, literally hanging"

### Solution 2A: Agent Self-Recovery

**Detect controller disconnect and reset to Idle state after timeout**

**Implementation Plan**:

1. **Add heartbeat timeout detection** (src/bin/agent.rs)
   ```rust
   // In stats writer loop (line 1138-1160)
   loop {
       tokio::select! {
           Some(req) = rx_workload.recv() => {
               // ... existing request handling ...
           }
           
           // 🆕 Periodic health check
           _ = tokio::time::sleep(Duration::from_secs(30)) => {
               // Check if controller is responsive
               if self.check_controller_health().await.is_err() {
                   error!("❌ Controller appears dead (no activity for 30s)");
                   error!("🔄 Initiating self-recovery: aborting workload and resetting to Idle");
                   
                   // Abort current workload
                   if let Err(e) = self.abort_workload().await {
                       error!("Failed to abort workload during recovery: {}", e);
                   }
                   
                   // Reset to Idle state
                   {
                       let mut state = self.state.write().await;
                       *state = WorkloadState::Idle;
                   }
                   
                   // Clear metrics
                   self.live_stats_aggregator.reset().await;
                   
                   warn!("✅ Agent reset to Idle state, ready for new connections");
                   
                   break;  // Exit this workload stream
               }
           }
       }
   }
   ```

2. **Add health check method**
   ```rust
   async fn check_controller_health(&self) -> Result<()> {
       // Check last stats send time
       let last_activity = self.live_stats_aggregator.last_update_time().await;
       let now = Instant::now();
       
       if now.duration_since(last_activity) > Duration::from_secs(60) {
           bail!("No stats updates sent in 60s - controller likely dead");
       }
       
       // Could also ping controller with keep-alive RPC
       // controller.ping().await?;
       
       Ok(())
   }
   ```

**Expected Outcome**: Agent detects controller death within 60s, self-recovers, ready for new connection.

### Solution 2B: Controller Retry Logic

**Don't exit on first error - retry with exponential backoff**

**Implementation Plan**:

1. **Add retry wrapper for agent connections** (src/bin/controller.rs)
   ```rust
   async fn connect_to_agent_with_retry(
       agent_addr: &str,
       max_retries: u32,
   ) -> Result<AgentClient> {
       let mut retries = 0;
       let mut backoff_ms = 1000;
       
       loop {
           match AgentClient::connect(agent_addr.to_string()).await {
               Ok(client) => {
                   info!("✅ Connected to agent at {}", agent_addr);
                   return Ok(client);
               }
               Err(e) => {
                   if retries >= max_retries {
                       return Err(anyhow!("Failed to connect to {} after {} retries: {}", 
                                         agent_addr, retries, e));
                   }
                   
                   warn!("⚠️  Connection attempt {} to {} failed: {}", 
                         retries + 1, agent_addr, e);
                   warn!("🔄 Retrying in {}ms...", backoff_ms);
                   
                   tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                   retries += 1;
                   backoff_ms = (backoff_ms * 2).min(30_000);  // Cap at 30s
               }
           }
       }
   }
   ```

2. **Add retry for stream errors during workload**
   ```rust
   // In run_distributed (controller.rs)
   async fn run_distributed(&self, config: &Config) -> Result<()> {
       let max_stream_retries = 3;
       let mut stream_retry = 0;
       
       loop {
           match self.execute_distributed_workload(config).await {
               Ok(results) => return Ok(results),
               
               Err(e) if stream_retry < max_stream_retries => {
                   error!("❌ Workload stream error (attempt {}/{}): {}", 
                          stream_retry + 1, max_stream_retries, e);
                   error!("🔄 Checking agent health and retrying...");
                   
                   // Check which agents are still alive
                   let alive_agents = self.check_agent_liveness().await?;
                   
                   if alive_agents.len() < self.agents.len() / 2 {
                       return Err(anyhow!("Lost majority of agents ({}/{}), cannot continue",
                                         self.agents.len() - alive_agents.len(),
                                         self.agents.len()));
                   }
                   
                   warn!("✅ {}/{} agents still alive, retrying workload...",
                         alive_agents.len(), self.agents.len());
                   
                   stream_retry += 1;
                   tokio::time::sleep(Duration::from_secs(5)).await;
               }
               
               Err(e) => {
                   error!("❌ Workload failed after {} retries: {}", stream_retry, e);
                   return Err(e);
               }
           }
       }
   }
   ```

**Expected Outcome**: Transient network errors don't kill entire workload, controller retries gracefully.

### Solution 2C: Agent Liveness Check RPC

**Add explicit "are you alive?" RPC for controller to query agents**

**Implementation Plan**:

1. **Add RPC to proto** (proto/iobench.proto)
   ```protobuf
   message PingRequest {
       uint64 timestamp_ms = 1;
   }
   
   message PingResponse {
       uint64 timestamp_ms = 1;      // Echo request timestamp
       uint64 agent_time_ms = 2;     // Agent's current time
       string state = 3;             // "Idle", "Running", "Failed", etc.
       uint64 uptime_secs = 4;       // How long agent has been running
   }
   
   service Agent {
       // ... existing RPCs ...
       
       // Health check / liveness probe
       rpc Ping(PingRequest) returns (PingResponse);
   }
   ```

2. **Implement in agent** (src/bin/agent.rs)
   ```rust
   async fn ping(
       &self,
       request: Request<PingRequest>,
   ) -> Result<Response<PingResponse>, Status> {
       let req = request.into_inner();
       let state = self.state.read().await;
       
       let response = PingResponse {
           timestamp_ms: req.timestamp_ms,
           agent_time_ms: SystemTime::now()
               .duration_since(UNIX_EPOCH)
               .unwrap()
               .as_millis() as u64,
           state: format!("{:?}", *state),
           uptime_secs: self.start_time.elapsed().as_secs(),
       };
       
       Ok(Response::new(response))
   }
   ```

3. **Use in controller** (src/bin/controller.rs)
   ```rust
   async fn check_agent_liveness(&self) -> Result<Vec<String>> {
       let mut alive_agents = Vec::new();
       
       for (agent_id, client) in &self.agent_clients {
           let request = PingRequest {
               timestamp_ms: SystemTime::now()
                   .duration_since(UNIX_EPOCH)
                   .unwrap()
                   .as_millis() as u64,
           };
           
           match tokio::time::timeout(
               Duration::from_secs(5),
               client.clone().ping(request)
           ).await {
               Ok(Ok(response)) => {
                   let resp = response.into_inner();
                   debug!("✅ Agent {} alive (state: {}, uptime: {}s)",
                          agent_id, resp.state, resp.uptime_secs);
                   alive_agents.push(agent_id.clone());
               }
               Ok(Err(e)) => {
                   warn!("❌ Agent {} ping RPC error: {}", agent_id, e);
               }
               Err(_) => {
                   warn!("❌ Agent {} ping timeout (5s)", agent_id);
               }
           }
       }
       
       Ok(alive_agents)
   }
   ```

**Expected Outcome**: Controller can detect dead/hung agents quickly, exclude them from workload.

---

## Problem 3: Cryptic Error Messages (HIGH PRIORITY)

### Current Behavior

**User sees**:
```
Error: h2 protocol error: error reading a body from connection: stream error received: stream no longer needed
```

**User thinks**: "WTF does this mean? Which agent? What operation? Why?"

### Solution 3: Human-Readable Error Context

**Wrap all errors with clear context explaining what was happening**

**Implementation Plan**:

1. **Add error context to TreeManifest creation**
   ```rust
   let manifest = create_tree_manifest_only(config, agent_id, num_agents, &base_uri)
       .with_context(|| format!(
           "Failed to create directory tree manifest: width={}, depth={}, {} total directories\n\
            This operation generates all directory paths in memory before file creation.\n\
            For large trees (depth ≥4), this can take 30-90 seconds.\n\
            Hint: Check if agents are sending progress updates (should see ~5000 dirs/update)",
           config.directory_structure.as_ref().unwrap().width,
           config.directory_structure.as_ref().unwrap().depth,
           // Calculate total: sum of width^level for level 1..depth
           (1..=config.directory_structure.as_ref().unwrap().depth)
               .map(|level| config.directory_structure.as_ref().unwrap().width.pow(level as u32))
               .sum::<usize>()
       ))?;
   ```

2. **Add context to gRPC stream errors**
   ```rust
   // In agent.rs execute_workload
   tx.send(Ok(stats)).await
       .map_err(|e| {
           Status::internal(format!(
               "❌ Failed to send LiveStats to controller\n\
                Agent: {} ({})\n\
                Error: {}\n\
                Possible causes:\n\
                  1. Controller process died or was killed\n\
                  2. Network connection lost\n\
                  3. Controller timed out waiting for updates\n\
                Next steps:\n\
                  - Agent will reset to Idle state after 60s\n\
                  - Check controller logs for errors\n\
                  - Verify network connectivity between hosts",
               self.id, self.address, e
           ))
       })?;
   ```

3. **Add context to prepare phase errors**
   ```rust
   prepare_sequential(...)
       .await
       .with_context(|| format!(
           "Failed during sequential prepare phase\n\
            Agent: {} ({} objects to create)\n\
            Backend: {}\n\
            Config: skip_verification={}, force_overwrite={}\n\
            Note: Prepare phase can take HOURS for large workloads.\n\
            Check LiveStats output to see current progress.",
           agent_id,
           config.ensure_objects.iter().map(|s| s.count).sum::<usize>(),
           config.ensure_objects.first().map(|s| &s.base_uri).unwrap_or(&"unknown".to_string()),
           config.skip_verification,
           config.force_overwrite
       ))?;
   ```

4. **Add timeout context to controller**
   ```rust
   // In controller.rs when waiting for agents
   tokio::time::timeout(Duration::from_secs(90), agent_stream.next())
       .await
       .with_context(|| format!(
           "❌ Timeout waiting for agent response (90s)\n\
            Agent: {}\n\
            Last known state: {:?}\n\
            Possible causes:\n\
              1. Agent is performing long operation without progress updates\n\
                 (e.g., TreeManifest creation for large directory structures)\n\
              2. Agent crashed or became unresponsive\n\
              3. Network connection lost\n\
              4. Agent is listing millions of objects (should use skip_verification=true)\n\
            Next steps:\n\
              1. Check agent logs for errors or 'Creating directory tree structure' message\n\
              2. If using directory_structure with depth ≥4, expect 30-90s delay\n\
              3. Consider enabling force_overwrite=true to skip object listing\n\
              4. Verify network connectivity: ping {}\n\
              5. Check if agent process is still running",
           agent_id,
           last_known_state,
           agent_address
       ))?;
   ```

**Expected Outcome**: User sees actionable error messages that explain:
- What operation failed
- Why it might have failed
- What to check
- How to fix it

---

## Problem 4: No Progress During Long Operations (HIGH PRIORITY)

### Current Behavior

**Prepare phase operations with ZERO progress reporting**:
1. TreeManifest creation (30-90s for 331k dirs) - **SILENT**
2. Object listing with millions of objects - **SILENT**
3. Directory creation (finalize_tree_with_mkdir) - Has progress bar, **NOT sent to controller**
4. Sequential prepare (file creation) - Updates every object, **GOOD**

### Solution 4: Add Progress to ALL Long Operations

**Implementation Plan**:

1. **TreeManifest creation** - DONE (see Solution 1A above)

2. **Object listing operations**
   ```rust
   // In list_existing_objects_distributed (prepare.rs)
   async fn list_existing_objects_distributed(
       store: &dyn ObjectStore,
       base_uri: &str,
       tree_manifest: &TreeManifest,
       live_stats_tracker: Option<Arc<LiveStatsTracker>>,
   ) -> Result<(usize, HashSet<usize>)> {
       let mut count = 0;
       let mut indices = HashSet::new();
       let report_interval = 10000;  // Every 10k objects
       
       // ... existing listing logic ...
       
       for (idx, obj_meta) in listed_objects.enumerate() {
           count += 1;
           indices.insert(extract_index_from_path(&obj_meta.location)?);
           
           // 🆕 Report progress periodically
           if count % report_interval == 0 {
               if let Some(tracker) = &live_stats_tracker {
                   tracker.set_prepare_progress(
                       count,
                       tree_manifest.total_files,  // Estimated total
                       format!("listing objects: {} found", count),
                   );
               }
               debug!("Listed {} objects so far...", count);
           }
       }
       
       // Final update
       if let Some(tracker) = &live_stats_tracker {
           tracker.set_prepare_progress(
               count,
               count,  // Actual count
               format!("listing complete: {} objects", count),
           );
       }
       
       Ok((count, indices))
   }
   ```

3. **Directory creation (finalize_tree_with_mkdir)**
   ```rust
   // In finalize_tree_with_mkdir (prepare.rs:2064+)
   for (idx, dir_path) in manifest.all_directories.iter().enumerate() {
       let full_uri = if base_uri.ends_with('/') {
           format!("{}{}", base_uri, dir_path)
       } else {
           format!("{}/{}", base_uri, dir_path)
       };
       
       store.mkdir(&full_uri).await
           .with_context(|| format!("Failed to create directory: {}", full_uri))?;
       
       metrics.mkdir_count += 1;
       metrics.mkdir.ops += 1;
       
       pb.inc(1);
       
       // 🆕 Send to controller every 5000 dirs
       if idx % 5000 == 0 {
           if let Some(tracker) = &live_stats_tracker {
               tracker.set_prepare_progress(
                   idx,
                   manifest.all_directories.len(),
                   format!("creating directories: {}/{}", idx, manifest.all_directories.len()),
               );
           }
       }
   }
   ```

**Expected Outcome**: Controller receives progress updates every few seconds for ALL long operations.

---

## Problem 5: Work Before Handshake (MEDIUM PRIORITY)

### Current Behavior

**In execute_workload RPC handler** (src/bin/agent.rs:1050+):
1. Receive request
2. Validate config
3. **Create TreeManifest (30-90s)** ← BLOCKS HERE
4. Transition to Running state
5. Start sending LiveStats

**Problem**: Step 3 happens before controller knows agent is responsive.

### Solution 5: Handshake-Then-Work Pattern

**Implementation Plan**:

1. **Send immediate ACK after config validation**
   ```rust
   async fn execute_workload(
       &self,
       request: Request<Streaming<WorkloadRequest>>,
   ) -> Result<Response<Self::ExecuteWorkloadStream>, Status> {
       let mut in_stream = request.into_inner();
       let (tx, rx) = mpsc::channel(128);
       
       // Receive initial request
       let Some(Ok(req)) = in_stream.next().await else {
           return Err(Status::invalid_argument("No initial request received"));
       };
       
       // Parse config
       let config: Config = serde_yaml::from_str(&req.config_yaml)
           .map_err(|e| Status::invalid_argument(format!("Invalid config: {}", e)))?;
       
       // 🆕 SEND IMMEDIATE ACK - controller knows we're alive
       let ack_stats = LiveStats {
           agent_id: self.id.clone(),
           sequence_number: 0,
           workload_state: WorkloadState::Running as i32,
           message: "Config validated, starting prepare phase...".to_string(),
           ..Default::default()
       };
       
       tx.send(Ok(ack_stats)).await
           .map_err(|e| Status::internal(format!("Failed to send ACK: {}", e)))?;
       
       // NOW do expensive operations (controller knows we're alive)
       let tree_manifest = if config.prepare_config.directory_structure.is_some() {
           Some(create_tree_manifest_with_progress(...).await?)
       } else {
           None
       };
       
       // ... rest of execution ...
   }
   ```

**Expected Outcome**: Controller receives ACK within 1-2s, knows agents are processing, won't timeout.

---

## Implementation Checklist

### Phase 1: Progress Reporting (Day 1)

- [ ] **TreeManifest Progress** (Solution 1A)
  - [ ] Update `create_tree_manifest_only()` signature to accept LiveStatsTracker
  - [ ] Modify `DirectoryTree::generate()` to send updates every 5000 dirs
  - [ ] Update all callers to pass tracker
  - [ ] Test with 24^4 = 331k directory config

- [ ] **Object Listing Progress** (Solution 4)
  - [ ] Add progress to `list_existing_objects_distributed()`
  - [ ] Report every 10k objects listed
  - [ ] Test with millions of objects

- [ ] **Directory Creation Progress** (Solution 4)
  - [ ] Add LiveStats updates to `finalize_tree_with_mkdir()`
  - [ ] Report every 5000 directories created
  - [ ] Test with 331k directory config

### Phase 2: Error Recovery (Day 2)

- [ ] **Agent Self-Recovery** (Solution 2A)
  - [ ] Add health check timer in execute_workload
  - [ ] Detect controller death (60s no activity)
  - [ ] Reset to Idle state on timeout
  - [ ] Test by killing controller mid-workload

- [ ] **Ping/Liveness RPC** (Solution 2C)
  - [ ] Add Ping RPC to proto
  - [ ] Implement in agent
  - [ ] Implement liveness check in controller
  - [ ] Test with network interruption

- [ ] **Controller Retry Logic** (Solution 2B)
  - [ ] Add retry wrapper for agent connections
  - [ ] Add retry for stream errors during workload
  - [ ] Exponential backoff (1s → 30s cap)
  - [ ] Test with transient network failures

### Phase 3: Error Messages (Day 3)

- [ ] **Add Context to All Errors** (Solution 3)
  - [ ] TreeManifest creation errors
  - [ ] gRPC stream send errors
  - [ ] Prepare phase errors
  - [ ] Controller timeout errors
  - [ ] Include: what failed, why, what to check, how to fix

- [ ] **Error Message Template**
  ```
  ❌ [Operation] failed
  Agent/Controller: [ID]
  Error: [technical error]
  Possible causes:
    1. [cause 1]
    2. [cause 2]
  Next steps:
    1. [action 1]
    2. [action 2]
  ```

### Phase 4: Handshake Improvements (Day 4)

- [ ] **Deferred Work** (Solution 1B, 5)
  - [ ] Send ACK immediately after config validation
  - [ ] Create TreeManifest AFTER ACK sent
  - [ ] Transition to Running before expensive ops
  - [ ] Test that controller receives ACK within 2s

- [ ] **Integration Testing**
  - [ ] 4-agent distributed mode
  - [ ] 331k directory tree
  - [ ] Kill controller mid-workload (agents should recover)
  - [ ] Network interruption (should retry)
  - [ ] Verify all error messages are human-readable

---

## Testing Plan

### Test Case 1: TreeManifest Timeout (CRITICAL)

**Config**:
```yaml
directory_structure:
  width: 24
  depth: 4  # 331,776 directories
  files_per_dir: 193  # 64,032,768 files total
```

**Before Fix**: h2 protocol timeout after 60-90s (no progress updates)

**After Fix**:
- ✅ Controller receives progress every ~2s (5000 dirs)
- ✅ No timeout
- ✅ Clear message: "Generating tree structure: 150000/331776 (45%)"

### Test Case 2: Controller Dies Mid-Workload (CRITICAL)

**Steps**:
1. Start 4-agent distributed workload
2. Wait for prepare phase to start
3. Kill controller process (Ctrl-C or `kill -9`)

**Before Fix**: Agents hang indefinitely, never recover

**After Fix**:
- ✅ Agents detect controller death within 60s
- ✅ Agents self-recover: abort workload, reset to Idle
- ✅ Clear message: "Controller appears dead, initiating self-recovery"
- ✅ Ready for new connections

### Test Case 3: Transient Network Error (HIGH)

**Steps**:
1. Start distributed workload
2. Introduce 5-second network partition (iptables DROP)
3. Restore network

**Before Fix**: Workload fails immediately on first error

**After Fix**:
- ✅ Controller retries connection (exponential backoff)
- ✅ Workload continues after network restored
- ✅ Clear messages: "Retrying in 2000ms..."

### Test Case 4: Human-Readable Errors (HIGH)

**Compare error messages**:

**Before**:
```
Error: h2 protocol error: stream error
```

**After**:
```
❌ Timeout waiting for agent response (90s)
Agent: agent1 (192.168.1.101:50051)
Last known state: Running
Possible causes:
  1. Agent is performing long operation without progress updates
     (e.g., TreeManifest creation for large directory structures)
  2. Agent crashed or became unresponsive
  3. Network connection lost
Next steps:
  1. Check agent logs for 'Creating directory tree structure' message
  2. If using directory_structure with depth ≥4, expect 30-90s delay
  3. Verify network connectivity: ping 192.168.1.101
  4. Check if agent process is still running
```

---

## Success Criteria

1. **No Timeouts**: 331k directory tree creates without h2 timeout
2. **Agent Recovery**: Agents reset to Idle within 60s of controller death
3. **Retry Works**: Transient network errors don't kill workload
4. **Clear Errors**: All error messages explain what/why/how-to-fix
5. **Progress Visibility**: Controller sees updates every 1-2s for all long operations
6. **Fast Handshake**: Agents ACK within 2s, start expensive work after

---

## Migration from v0.8.24 to v0.8.24a

**Breaking Changes**: NONE (backward compatible)

**New Features**:
- Progress reporting during TreeManifest creation
- Agent self-recovery on controller death
- Ping/liveness RPC
- Retry logic with exponential backoff
- Human-readable error messages
- Immediate ACK before expensive operations

**Config Changes**: NONE (all improvements are automatic)

**Deployment**:
1. Build new binaries: `cargo build --release`
2. Deploy controller: `./target/release/sai3bench-ctl`
3. Deploy agents: `./target/release/sai3bench-agent`
4. No config changes required
5. Test with existing 4host-test_config.yaml

---

## Future Work (Deferred to v0.8.25)

See **DISTRIBUTED_BARRIER_SYNC_DESIGN.md** for:
- ✅ Full barrier synchronization between phases
- ✅ Heartbeat-based liveness (30s intervals)
- ✅ Phase-specific barrier modes (All/Majority/BestEffort)
- ✅ New agent state machine with phase granularity
- ✅ Barrier coordination RPCs
- ✅ Configurable barrier timeouts per phase

**Estimated effort**: 3-4 weeks (tracked separately)

---

## Questions for User

1. **Retry limits**: How many times should controller retry before giving up? (Suggest: 3 retries for streams, infinite for connections with user interrupt)

2. **Recovery timeout**: 60s for agent self-recovery OK? Or prefer faster (30s) or slower (90s)?

3. **Progress interval**: 5000 directories OK? Or want more frequent (1000) or less (10000)?

4. **Error verbosity**: Current proposal is VERY verbose - is this OK for production logs?

---

## Summary

This handoff targets **immediate production stability** before implementing full barrier synchronization:

**Top Priority (CRITICAL)**:
1. TreeManifest progress reporting (fixes timeout)
2. Agent self-recovery (fixes hang on controller death)

**High Priority**:
3. Human-readable errors (improves debugging)
4. Retry logic (improves robustness)

**Medium Priority**:
5. Immediate ACK handshake (improves UX)
6. Progress for all long operations (improves visibility)

**Estimated Timeline**: 3-4 days for complete implementation + testing

**User Impact**: Production-ready distributed mode with graceful error handling and clear feedback.
