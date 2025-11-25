# Docker Production Deployment Guide

## Problem: SSH Disconnection During Long Tests

When running distributed tests in cloud environments, you face a dilemma:
- **Interactive mode** (`-it`): See output, but SSH disconnect kills test
- **Daemon mode** (`-d`): Survives SSH disconnect, but no visibility

## Solution: Daemon Mode + File Logging + tmux

### Agent Deployment (Daemon with Logs)

```bash
#!/bin/bash
# run_sai3-agent.sh - Improved version with logging

AGENT_ID="${1:-agent-1}"
PORT="${2:-7761}"
LOG_DIR="/home/ubuntu/sai3-logs"
mkdir -p "$LOG_DIR"

# Stop any existing agent
docker rm -f sai3-agent-${AGENT_ID} 2>/dev/null

# Start agent in daemon mode with logging
docker run -d \
    --name sai3-agent-${AGENT_ID} \
    --net=host \
    -v /home/ubuntu:/home/ubuntu \
    -v ${LOG_DIR}:/logs \
    -e AWS_SECRET_ACCESS_KEY \
    -e AWS_ACCESS_KEY_ID \
    -e AWS_REGION \
    sai3-tools \
    bash -c "/usr/local/bin/sai3bench-agent -v --listen 0.0.0.0:${PORT} 2>&1 | tee /logs/agent-${AGENT_ID}.log"

echo "Agent ${AGENT_ID} started on port ${PORT}"
echo "View logs: docker logs -f sai3-agent-${AGENT_ID}"
echo "Or: tail -f ${LOG_DIR}/agent-${AGENT_ID}.log"
```

### Controller Deployment (tmux + Daemon with Logs)

```bash
#!/bin/bash
# run_sai3-controller.sh - Improved version with tmux and logging

CONFIG="${1:-./config.yaml}"
LOG_DIR="/home/ubuntu/sai3-logs"
mkdir -p "$LOG_DIR"

# Check if already in tmux
if [ -z "$TMUX" ]; then
    echo "ERROR: Must run inside tmux session!"
    echo "Start tmux first: tmux new -s sai3-test"
    exit 1
fi

# Stop any existing controller
docker rm -f sai3-controller 2>/dev/null

echo "Starting controller in foreground mode (inside tmux)..."
echo "Logs will be saved to: ${LOG_DIR}/controller.log"
echo ""

# Run interactively (we're inside tmux, so SSH disconnect is OK)
docker run -it --rm \
    --name sai3-controller \
    --net=host \
    -v /home/ubuntu:/home/ubuntu \
    -v ${LOG_DIR}:/logs \
    -e AWS_SECRET_ACCESS_KEY \
    -e AWS_ACCESS_KEY_ID \
    -e AWS_REGION \
    sai3-tools \
    bash -c "/usr/local/bin/sai3bench-ctl run --config ${CONFIG} 2>&1 | tee /logs/controller.log"
```

### Complete Deployment Workflow

#### 1. On Each Agent Host (8 hosts in your case)

```bash
# SSH to agent host
ssh ubuntu@agent-host-1

# Start agent (adjust port if needed)
./run_sai3-agent.sh agent-1 7761
```

**Verify agent started:**
```bash
docker logs -f sai3-agent-agent-1
# Should see: "sai3bench-agent listening (PLAINTEXT) on 0.0.0.0:7761"
```

#### 2. On Controller Host

```bash
# SSH to controller host
ssh ubuntu@controller-host

# Create or attach to tmux session
tmux new -s sai3-test
# OR if session exists: tmux attach -t sai3-test

# Inside tmux: Start controller
./run_sai3-controller.sh ./resnet50_8-hosts.yaml

# Watch it run...
# If SSH disconnects, reconnect and: tmux attach -t sai3-test
```

### Monitoring During Execution

**From another terminal (or after SSH reconnect):**

```bash
# Controller output
tail -f /home/ubuntu/sai3-logs/controller.log

# Agent outputs (on each agent host)
tail -f /home/ubuntu/sai3-logs/agent-*.log

# All docker containers
docker ps

# Agent CPU/memory
docker stats sai3-agent-agent-1
```

### Handling SSH Disconnections

#### If Controller SSH Disconnects:
1. SSH back to controller host
2. `tmux attach -t sai3-test`
3. You're back - test still running!

#### If Agent SSH Disconnects:
Agents run in daemon mode - they keep running. Check logs:
```bash
docker logs -f sai3-agent-agent-1
# OR
tail -f /home/ubuntu/sai3-logs/agent-1.log
```

### Cleanup After Test

```bash
# Stop all agents (on each agent host)
docker rm -f sai3-agent-agent-1

# Controller stops automatically when test completes
# If you need to force stop:
docker rm -f sai3-controller

# Clean up logs (optional)
rm -rf /home/ubuntu/sai3-logs
```

## Network Interruption Resilience

### Current Behavior (v0.8.5)
- If one agent loses connection, **entire test fails**
- Controller aborts all other agents immediately
- This is too aggressive for transient network issues

### Recommended Enhancement (Future)
Consider adding:
- **Retry logic**: Attempt to reconnect to disconnected agents
- **Degraded mode**: Continue with N-1 agents if one fails
- **Timeout threshold**: Only abort if agent silent for >60s

### Workaround for Now
1. **Test network stability first**: Ping all agents continuously
   ```bash
   # On controller host
   for i in {1..8}; do ping -c 5 agent-host-$i & done
   ```

2. **Use reliable network**: AWS VMs in same VPC/region have very low packet loss

3. **Increase timeouts**: If you see spurious failures, adjust in code:
   - `AGENT_READY_TIMEOUT_SECS` (default: 70s)
   - `WORKLOAD_MAX_DURATION_SECS` (default: 3600s)

## Advanced: Log Aggregation

For 8+ agents, consider aggregating logs:

```bash
# On controller host, create log collector
#!/bin/bash
# collect-agent-logs.sh
LOG_DIR="/home/ubuntu/sai3-logs"
AGENTS="agent-1 agent-2 agent-3 agent-4 agent-5 agent-6 agent-7 agent-8"
AGENT_HOSTS="host1 host2 host3 host4 host5 host6 host7 host8"

while true; do
    for i in {1..8}; do
        agent=$(echo $AGENTS | cut -d' ' -f$i)
        host=$(echo $AGENT_HOSTS | cut -d' ' -f$i)
        
        # Pull latest logs via SSH
        ssh ubuntu@$host "tail -50 /home/ubuntu/sai3-logs/${agent}.log" \
            > ${LOG_DIR}/remote-${agent}.log 2>/dev/null
    done
    sleep 5
done
```

## Troubleshooting

### "Container Exited Immediately"
```bash
docker logs sai3-agent-agent-1
# Check for errors like missing credentials, port conflicts
```

### "Cannot Connect to Agent"
```bash
# From controller host
nc -zv agent-host-1 7761
# Should show: Connection to agent-host-1 7761 port [tcp/*] succeeded!
```

### "Permission Denied" on Log Files
```bash
sudo chown -R ubuntu:ubuntu /home/ubuntu/sai3-logs
sudo chmod -R 755 /home/ubuntu/sai3-logs
```

### "Agent Shows Old Test Output"
Restart agent container:
```bash
docker rm -f sai3-agent-agent-1
./run_sai3-agent.sh agent-1 7761
```

## Best Practices Summary

1. ✅ **Agents**: Run in daemon mode (`-d`) with file logging
2. ✅ **Controller**: Run in tmux with interactive mode (`-it`) + file logging
3. ✅ **Logs**: Save to mounted volume for post-mortem analysis
4. ✅ **Monitoring**: Use `tail -f` on log files for real-time view
5. ✅ **Network**: Test connectivity before starting distributed workload
6. ✅ **Cleanup**: Remove containers after each test to avoid confusion

## Next Steps

After deployment, if you still see network errors:
1. Check `/home/ubuntu/sai3-logs/controller.log` for full error context
2. Check each agent's log for network timeouts
3. Consider filing issue for "graceful agent reconnection" feature
