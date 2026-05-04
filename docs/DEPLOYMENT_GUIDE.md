# Distributed Deployment Guide

Complete guide for deploying sai3-bench across multiple hosts using SSH, containers, or both.

## Table of Contents

1. [SSH Deployment (Bare Metal/VMs)](#ssh-deployment)
2. [Container Deployment](#container-deployment)
3. [Production Best Practices](#production-best-practices)

---

## SSH Deployment

### Quick Start: Automated SSH Setup

The `ssh-setup` command automates passwordless SSH configuration:

```bash
# One command to setup all VMs:
sai3bench-ctl ssh-setup --hosts ubuntu@vm1.example.com,ubuntu@vm2.example.com,ubuntu@vm3.example.com

# Or with IP addresses:
sai3bench-ctl ssh-setup --hosts ubuntu@10.0.1.10,ubuntu@10.0.1.11,ubuntu@10.0.1.12

# Default user:
sai3bench-ctl ssh-setup --hosts vm1,vm2,vm3 --user ubuntu
```

**What it does:**

1. Generates SSH key pair (`~/.ssh/sai3bench_id_rsa`)
2. Distributes public key to each VM (prompts for password once)
3. Verifies passwordless access
4. Checks for binaries/Docker on each VM
5. Generates config template

**Example output:**

```text
=== Setting up SSH access to ubuntu@vm1.aws.com ===
✓ SSH key generated: /home/user/.ssh/sai3bench_id_rsa
✓ SSH key copied to vm1.aws.com
✓ Passwordless SSH access verified
✓ sai3bench-agent found: /usr/local/bin/sai3bench-agent
✓ Host vm1.aws.com is ready

=== Setup Summary ===
✓ Successfully configured: 3/3

=== Next Steps ===
1. Update your workload YAML:
   distributed:
     ssh:
       enabled: true
       user: ubuntu
       key_path: /home/user/.ssh/sai3bench_id_rsa
     agents:
       - address: vm1.aws.com
       - address: vm2.aws.com
       - address: vm3.aws.com

2. Run distributed test:
   sai3bench-ctl run --config workload.yaml
```

### Manual SSH Setup (Advanced)

If you prefer manual setup:

```bash
# 1. Generate SSH key
ssh-keygen -t rsa -b 4096 -f ~/.ssh/sai3bench_id_rsa -N ""

# 2. Copy to each host
ssh-copy-id -i ~/.ssh/sai3bench_id_rsa.pub ubuntu@vm1.example.com
ssh-copy-id -i ~/.ssh/sai3bench_id_rsa.pub ubuntu@vm2.example.com

# 3. Install sai3bench on each host
scp target/release/sai3bench-agent ubuntu@vm1.example.com:/usr/local/bin/
ssh ubuntu@vm1.example.com "chmod +x /usr/local/bin/sai3bench-agent"

# 4. Test connection
ssh -i ~/.ssh/sai3bench_id_rsa ubuntu@vm1.example.com echo OK
```

### SSH Configuration

Add to your workload YAML:

```yaml
distributed:
  ssh:
    enabled: true
    user: ubuntu
    key_path: /home/user/.ssh/sai3bench_id_rsa
  agents:
    - address: vm1.example.com
      id: agent-1
    
    - address: vm2.example.com
      id: agent-2
```

The controller will automatically:

- SSH to each host
- Start `sai3bench-agent` in background
- Connect via gRPC
- Collect results
- Stop agents when done

---

## Container Deployment

### Option 1: Host Network Mode (Recommended)

Simplest setup - agents bind directly to host ports:

**On each agent host:**

```bash
docker run -d \
  --name sai3-agent \
  --net=host \
  -e AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID}" \
  -e AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY}" \
  -e AWS_REGION=us-east-1 \
  -e RUST_LOG=info \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167
```

**Check status:**

```bash
docker logs sai3-agent
# Should show: "sai3bench-agent listening (PLAINTEXT) on 0.0.0.0:7167"
```

**Controller config:**

```yaml
distributed:
  agents:
    - address: "vm1.example.com:7167"
      id: "agent-1"
    
    - address: "vm2.example.com:7167"
      id: "agent-2"
```

### Option 2: Port Mapping

If host network is unavailable:

```bash
docker run -d \
  --name sai3-agent \
  -p 7167:7167 \
  -e AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID}" \
  -e AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY}" \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167
```

### Passing Cloud Credentials

**AWS:**

```bash
docker run -d --net=host \
  -e AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID}" \
  -e AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY}" \
  -e AWS_REGION=us-east-1 \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167
```

**Azure:**

```bash
docker run -d --net=host \
  -e AZURE_STORAGE_ACCOUNT="${AZURE_STORAGE_ACCOUNT}" \
  -e AZURE_STORAGE_KEY="${AZURE_STORAGE_KEY}" \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167
```

**GCS (via service account file):**

```bash
docker run -d --net=host \
  -v /path/to/gcs-key.json:/gcs-key.json:ro \
  -e GOOGLE_APPLICATION_CREDENTIALS=/gcs-key.json \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167
```

### Building Container Image

```dockerfile
# Dockerfile
FROM rust:1.75 as builder
WORKDIR /build
COPY . .
RUN cargo build --release

FROM ubuntu:22.04
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/sai3bench-agent /usr/local/bin/
COPY --from=builder /build/target/release/sai3bench-ctl /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/sai3bench-agent"]
```

```bash
docker build -t myregistry/sai3-bench:v0.8.22 .
docker push myregistry/sai3-bench:v0.8.22
```

---

## Production Best Practices

### Problem: SSH Disconnection During Long Tests

When running distributed tests in cloud environments:

- **Interactive mode** (`-it`): See output, but SSH disconnect kills test
- **Daemon mode** (`-d`): Survives disconnect, but no visibility

### Solution: tmux + Daemon Agents + File Logging

#### 1. Start Agents in Daemon Mode with Logging

Create `run_agent.sh` on each agent host:

```bash
#!/bin/bash
AGENT_ID="${1:-agent-1}"
PORT="${2:-7167}"
LOG_DIR="/home/ubuntu/sai3-logs"
mkdir -p "$LOG_DIR"

# Stop existing
docker rm -f sai3-agent-${AGENT_ID} 2>/dev/null

# Start with logging
docker run -d \
    --name sai3-agent-${AGENT_ID} \
    --net=host \
    -v ${LOG_DIR}:/logs \
    -e AWS_ACCESS_KEY_ID \
    -e AWS_SECRET_ACCESS_KEY \
    -e AWS_REGION \
    myregistry/sai3-bench:latest \
    bash -c "sai3bench-agent --listen 0.0.0.0:${PORT} 2>&1 | tee /logs/agent-${AGENT_ID}.log"

echo "Agent started: docker logs -f sai3-agent-${AGENT_ID}"
echo "Or: tail -f ${LOG_DIR}/agent-${AGENT_ID}.log"
```

Run on each host:

```bash
./run_agent.sh agent-1 7167
```

#### 2. Run Controller in tmux Session

On controller host:

```bash
# Create tmux session (survives SSH disconnect)
tmux new -s sai3-test

# Run controller
sai3bench-ctl run --config distributed-test.yaml

# Detach: Ctrl-B, then D
# Reattach later: tmux attach -t sai3-test
```

#### 3. Monitor Progress

While controller runs:

**View live stats:**

```bash
# Controller output shows aggregate stats every second
# Ctrl-B, then D to detach without stopping
```

**Check agent logs:**

```bash
# SSH to agent host
tail -f /home/ubuntu/sai3-logs/agent-1.log

# Or via Docker
docker logs -f sai3-agent-agent-1
```

**Check running status:**

```bash
# List all running agents
docker ps | grep sai3-agent
```

### Multi-Region Cloud Deployment

For cross-region testing:

**Agent hosts in different regions:**

```yaml
distributed:
  agents:
    # US East
    - address: "10.0.1.10:7167"
      id: "agent-us-east-1"
    
    # US West
    - address: "10.0.2.10:7167"
      id: "agent-us-west-1"
    
    # Europe
    - address: "10.0.3.10:7167"
      id: "agent-eu-west-1"

# Each agent can target different endpoint
workload:
  - op: get
    path: "data/*.dat"
    weight: 100
```

**Region-specific configurations:**

```bash
# US East agent
docker run -d --net=host \
  -e AWS_REGION=us-east-1 \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167

# EU West agent
docker run -d --net=host \
  -e AWS_REGION=eu-west-1 \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7167
```

### Firewall/Security Group Configuration

**Required ports:**

- Agent: TCP 7167 (or custom port) - inbound from controller
- Controller: No inbound required (initiates connections)

**AWS Security Group example:**

```text
Type: Custom TCP
Port: 7167
Source: <controller-security-group-id>
Description: sai3-bench agent gRPC
```

**Verify connectivity:**

```bash
# From controller
telnet vm1.example.com 7167
# Should connect (then Ctrl-C to exit)
```

### Cleanup

**Stop all agents:**

```bash
# Via Docker
docker stop sai3-agent
docker rm sai3-agent

# Via SSH (if controller started them)
# Agents automatically stop when controller disconnects
```

**Remove logs:**

```bash
rm -rf /home/ubuntu/sai3-logs
```

---

## Troubleshooting

### Agent won't start

**Check logs:**

```bash
docker logs sai3-agent
```

**Common issues:**

- Port already in use: `lsof -i :7167`
- Missing credentials: verify env vars are set
- Network: ensure port is accessible from controller

### Controller can't connect to agent

**Test network:**

```bash
telnet agent-host 7167
```

**Check agent is listening:**

```bash
docker logs sai3-agent | grep "listening"
# Should show: "sai3bench-agent listening (PLAINTEXT) on 0.0.0.0:7167"
```

**Verify credentials:**

```bash
docker exec sai3-agent env | grep AWS
```

### SSH timeout/disconnection

**Use tmux:**

```bash
tmux new -s sai3-test
sai3bench-ctl run --config test.yaml
# Ctrl-B, D to detach
```

**Monitor from outside tmux:**

```bash
tail -f sai3-*/results.tsv
```

### Results not appearing

**Check controller output:**

- Look for "Results saved to:" message
- Verify directory was created

**Check agent results:**

```bash
docker exec sai3-agent ls -lh /tmp/sai3-*
```

---

## Credential Forwarding (v0.8.92+)

In a distributed run every agent host must have object-storage credentials
available before it can validate or run a workload. Starting with v0.8.92,
`sai3bench-ctl` can **forward** credential environment variables to all agents
automatically. The credentials travel inside the existing gRPC control channel
and are applied to each agent's process environment before any storage
operations begin.

### Quick Start

**Option A — Forward from the controller's own environment** (default)

```bash
export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY

# Credentials are forwarded to all agents automatically
sai3bench-ctl run --config workload.yaml
```

**Option B — Use a dedicated credentials file**

```bash
# /etc/sai3bench/creds.env
AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
AWS_DEFAULT_REGION=us-east-1
```

```bash
sai3bench-ctl --env-file /etc/sai3bench/creds.env run --config workload.yaml
```

The file is read on the controller; its contents are **not** applied to the
controller's own environment.

**Option C — Disable forwarding (agents self-configure)**

```bash
sai3bench-ctl --no-forward-env run --config workload.yaml
```

Use this when agents already have credentials configured locally (e.g. via IAM
instance roles or Kubernetes secrets) and you do not want the controller to
interfere.

### Security Model

**Allow-list**: Only these prefixes are ever forwarded. All other variables are
silently ignored, even when present in an `--env-file`.

| Prefix / Key | Cloud / SDK |
|---|---|
| `AWS_*` | AWS SDK (access key, secret, session token, region, endpoint URL, …) |
| `GOOGLE_APPLICATION_CREDENTIALS` | GCP — service account JSON key path |
| `AZURE_STORAGE_*` | Azure Blob Storage (account name, key, SAS token, connection string) |

**Local environment wins**: If a key already exists in the agent's own
environment (e.g. an instance IAM role sets `AWS_ROLE_ARN`), the forwarded
value is silently skipped.

**Audit trail**: The controller logs the key names (never values) it is forwarding:

```text
🔑 Forwarding 2 credential variables from controller environment to all agents: AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
```

**TLS warning**: Credentials are embedded in the gRPC control message. When
the connection is plaintext (no `--tls`), the controller emits a warning.
Enable `--tls` and provide `--agent-ca` for production deployments on shared
networks.

**Never written to disk**: The `distributed_env` field that carries credentials
uses `skip_serializing_if = "is_empty"` — it never appears in YAML config
files, dry-run output, or results saved to disk.

### CLI Reference

```text
sai3bench-ctl [OPTIONS] run --config <FILE>

  --env-file <FILE>     Path to a .env file whose credential variables are
                        forwarded to every agent.
  --no-forward-env      Disable all credential forwarding.
  --tls                 Enable TLS for gRPC connections.
  --agent-ca <FILE>     PEM file for the agent's self-signed certificate (TLS).
```

### `.env` File Format

Standard `key=value` format; comments (`# ...`) and blank lines supported:

```dotenv
# AWS credentials
AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
AWS_DEFAULT_REGION=us-east-1

# Custom S3-compatible endpoint (e.g. MinIO)
AWS_ENDPOINT_URL=http://10.9.0.1:9000

# GCP
# GOOGLE_APPLICATION_CREDENTIALS=/etc/gcp/sa-key.json

# Azure
# AZURE_STORAGE_ACCOUNT=mystorageaccount
# AZURE_STORAGE_KEY=base64encodedkey==
```

Non-credential lines are silently ignored — they are parsed but not forwarded.

### Credential Forwarding Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `403 Forbidden` / `AccessDenied` during pre-flight | Credentials not forwarded or wrong | Check controller log for "Forwarding N credential variables" |
| `No credentials detected` warning | Controller env has no matching keys and no `--env-file` | Set `AWS_ACCESS_KEY_ID` on controller or pass `--env-file` |
| Agent ignores forwarded credentials | Agent already has the same key set locally | Local env wins by design; unset the local key if you want the forwarded value |
| `Cannot read env file: …` error | `--env-file` path is wrong or not readable | Verify path and file permissions |

---

## See Also

- [USAGE.md](USAGE.md) - Workload configuration syntax
- [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) - Distributed testing concepts
- [CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md) - Cloud storage configuration
