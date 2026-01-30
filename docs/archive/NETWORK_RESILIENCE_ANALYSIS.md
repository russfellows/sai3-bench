# Network Resilience Analysis - sai3-bench v0.8.5

## Your Test Failure Analysis

### What Happened

```
❌ Agent agent-6 failed: Agent workload failed: status: Unknown, 
   message: "h2 protocol error: error reading a body from connection"
```

**Timeline:**
1. All 8 agents started successfully
2. All 8 agents validated config and sent READY
3. Controller sent coordinated START
4. Prepare phase began (creating 4,915,200 objects across 8 agents)
5. Progress: 54% complete (2,657,652/4,915,200 objects created)
6. **Agent-6 network connection dropped** (SSH timeout, VM restart, network glitch, etc.)
7. Controller detected agent-6 stream error
8. Controller **immediately aborted all 7 remaining healthy agents**
9. Test failed with 0 operations completed (prepare phase doesn't count as "operations")

### Why This Happened

**Root cause**: Network interruption on agent-6 host.

Possible causes:
- SSH connection timeout (your suspicion - likely correct)
- AWS network maintenance/rebalancing
- Agent VM restart or resource exhaustion
- gRPC keepalive timeout (default: 2 hours, but can be affected by intermediate proxies)

**Current controller behavior**: Fail-fast on any agent error.

## Current Design Philosophy (v0.8.5)

### Fail-Fast Strategy

The controller uses **fail-fast** approach:
```rust
// src/bin/controller.rs, line 1800
if stats.status == 3 {  // ERROR status
    abort_all_agents(...).await;
    anyhow::bail!("Agent {} workload execution failed: {}", stats.agent_id, stats.error_message);
}
```

**Rationale:**
1. **Data consistency**: Distributed workload results depend on all agents
2. **Reproducibility**: Partial results may be misleading
3. **Early detection**: Catch configuration/permission errors quickly
4. **Resource efficiency**: Don't waste time if test is already compromised

**Drawback:**
- Transient network issues destroy long-running tests
- 7 healthy agents get killed because 1 had network glitch
- No retry mechanism for recoverable errors

## Alternative Approaches

### Option 1: Best-Effort Continuation (Graceful Degradation)

Allow test to continue with N-1 agents when one fails:

**Pros:**
- Tolerates transient network issues
- Useful for availability testing
- Partial results better than no results
- Natural for "at least N agents" workloads

**Cons:**
- Results may not be comparable across runs
- Harder to detect configuration errors
- Need to mark results as "partial"
- Agent assignment becomes unbalanced

**Implementation complexity**: Medium (1-2 days)

### Option 2: Retry with Exponential Backoff

Attempt to reconnect to failed agents before aborting:

**Pros:**
- Handles transient network glitches
- Maintains full agent count if recoverable
- Fail-fast still happens for permanent failures
- Most robust approach

**Cons:**
- Adds retry latency (could be 30-60s)
- More complex state machine
- Need to handle partial prepare phase completion
- Agents must be idempotent

**Implementation complexity**: High (3-5 days)

### Option 3: Configurable Tolerance Policy

Add config option for failure handling:

```yaml
distributed:
  failure_policy: "fail_fast"  # or "best_effort", "retry_once", "continue_if_majority"
  min_agents_required: 6       # Continue with at least 6/8 agents
  agent_retry_attempts: 2
  agent_retry_delay_s: 10
```

**Pros:**
- Flexible for different use cases
- Can be strict for correctness testing, lenient for throughput testing
- User controls trade-offs

**Cons:**
- Most complex to implement
- Need to validate policy makes sense
- Documentation burden

**Implementation complexity**: High (4-6 days)

## Recommended Short-Term Workarounds

Since implementing retry logic is complex, here are immediate solutions:

### 1. Prevent Network Interruptions (Best)

**SSH connection management:**
```bash
# On your local machine, edit ~/.ssh/config:
Host *.amazonaws.com
    ServerAliveInterval 60
    ServerAliveCountMax 10
    TCPKeepAlive yes
```

This sends keepalive every 60s, allowing 10 missed keepalives (10 minutes of unresponsiveness) before disconnect.

**tmux on controller** (already recommended):
- Even if SSH dies, controller keeps running
- Reconnect with `tmux attach -t sai3-test`
- See full output from beginning

**Agents in daemon mode** (already recommended):
- Agents don't care about SSH - they're HTTP servers
- They keep running even if you disconnect

### 2. Pre-Test Network Validation

Add network stability check before starting distributed workload:

```bash
#!/bin/bash
# validate-network.sh

AGENTS="agent-host-1 agent-host-2 agent-host-3 agent-host-4 agent-host-5 agent-host-6 agent-host-7 agent-host-8"
PORTS="7761 7762 7763 7764 7765 7766 7767 7768"

echo "=== Network Validation ==="
echo "Testing connectivity to all agents..."

failed=0
for agent in $AGENTS; do
    echo -n "  $agent... "
    if ping -c 3 -W 2 $agent >/dev/null 2>&1; then
        echo "✅ reachable"
    else
        echo "❌ UNREACHABLE"
        ((failed++))
    fi
done

if [ $failed -gt 0 ]; then
    echo ""
    echo "❌ $failed agent(s) unreachable - fix network before starting test"
    exit 1
fi

echo ""
echo "Testing gRPC ports..."
failed=0
idx=0
for agent in $AGENTS; do
    port=$(echo $PORTS | cut -d' ' -f$((++idx)))
    echo -n "  $agent:$port... "
    if nc -z -w 2 $agent $port >/dev/null 2>&1; then
        echo "✅ open"
    else
        echo "❌ CLOSED"
        ((failed++))
    fi
done

if [ $failed -gt 0 ]; then
    echo ""
    echo "❌ $failed port(s) not reachable - ensure agents are running"
    exit 1
fi

echo ""
echo "✅ All network checks passed - safe to start distributed workload"
```

### 3. Shorter Tests First

Instead of one long test, run multiple shorter tests:
- 5 x 60s tests instead of 1 x 300s test
- Reduces chance of network interruption
- Easier to restart if failure occurs
- Can aggregate results afterward

### 4. Increase gRPC Keepalive

gRPC has built-in keepalive, but defaults are conservative. You can tune them, but this requires code changes:

```rust
// In controller.rs, when creating gRPC channel:
let channel = tonic::transport::Channel::from_shared(uri)?
    .http2_keep_alive_interval(Duration::from_secs(30))  // Send keepalive every 30s
    .keep_alive_timeout(Duration::from_secs(10))         // Timeout if no response in 10s
    .keep_alive_while_idle(true)                          // Send even when no RPCs
    .connect()
    .await?;
```

**Caveat**: This adds overhead (more packets) and may not help with SSH interruptions.

## Long-Term Solution Recommendation

Based on your use case (long-running distributed tests in cloud environments), I recommend:

**Phase 1** (Immediate - you can do this now):
1. ✅ Use tmux for controller (survives SSH disconnect)
2. ✅ Use daemon mode + logging for agents
3. ✅ Add SSH keepalive configuration
4. ✅ Run network validation before each test

**Phase 2** (Code changes - 2-3 days):
1. Add retry logic with exponential backoff (2 retries, 10s delay)
2. Add `--continue-on-agent-failure` CLI flag for best-effort mode
3. Mark results as "PARTIAL" if any agent fails

**Phase 3** (Future enhancement - 1 week):
1. Implement full reconnection support
2. Agents can resume from last checkpoint
3. Controller tracks disconnect/reconnect events
4. Detailed network health metrics in results

## What To Do Right Now

### For Your Current Test Setup

1. **Add SSH keepalive** to prevent disconnections:
   ```bash
   vi ~/.ssh/config
   # Add the settings from "Short-Term Workarounds #1" above
   ```

2. **Use the Docker deployment scripts** from DOCKER_PRODUCTION_DEPLOYMENT.md:
   - Agents in daemon mode with logging
   - Controller in tmux with logging
   - Both survive SSH disconnects

3. **Run network validation** before each test

4. **If you still hit this issue**, the question becomes:
   - Do you want me to implement retry logic? (2-3 days work)
   - Or implement "continue with N-1 agents"? (1-2 days work)
   - Or is prevention good enough for now?

### Testing the Fix We Just Made

The spurious error fix (INFO instead of ERROR after completion) won't help with your current issue, because your agent failed **during** the workload (54% through prepare), not after completion.

However, you should still commit that fix because it eliminates false positives in logs.

## Questions for You

1. **How often does this happen?** 
   - Once per 10 tests? Once per 100 tests?
   - Helps prioritize whether retry logic is worth implementing

2. **How important is exact agent count?**
   - If testing storage backend, maybe 7/8 agents is good enough?
   - If testing synchronization, you need all 8 agents

3. **Test duration tolerance?**
   - Would you accept 30s retry delay if it means test continues?
   - Or prefer fail-fast and manual restart?

4. **Your priority?**
   - Fix network (cheaper, faster) vs implement retry (more robust, slower)

Let me know your answers and I can either:
- Help you perfect the network setup (no code changes)
- Implement retry logic (code changes, testing required)
- Implement best-effort continuation (code changes, simpler than retry)
