# io-bench — Usage Guide

io-bench is a multi-protocol I/O benchmarking suite with optional distributed execution via gRPC. It ships four binaries:

- **`io-bench`** — single-node CLI (health/list/stat/get/put/delete/run/replay) using `s3dlio`
- **`sai3bench-agent`** — per-host gRPC agent that runs I/O operations on that host
- **`sai3bench-ctl`** — controller that coordinates one or more agents
- **`sai3bench-run`** — legacy workload runner (being integrated into io-bench)

**Supported Backends**: `file://`, `direct://`, `s3://`, `az://`, `gs://`

This doc focuses on the distributed controller/agent mode, including plaintext and TLS (self‑signed) operation.

---

# Prerequisites

- **Rust toolchain** (stable, 2024 edition)  
- **Protobuf compiler** (`protoc`) — required by `tonic-build`  
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
- **Storage Backend Credentials** on each agent host:
  - **AWS S3**: The agent uses the AWS SDK default chain. Ensure one of:
    - `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (+ optional `AWS_SESSION_TOKEN`)
    - or `~/.aws/credentials` with a default or selected profile
    - or `./.env` file containing ACCESS_KEY_ID, SECRET_ACCESS_KEY and other required params
  - **Azure Blob Storage**: Requires environment variables:
    - `AZURE_STORAGE_ACCOUNT="your-storage-account-name"`
    - `AZURE_STORAGE_ACCOUNT_KEY="your-account-key"`
  - **Google Cloud Storage**: Application Default Credentials via gcloud CLI:
    - `gcloud auth application-default login`
  - **File/Direct I/O**: No credentials required, uses local filesystem
- **Open firewall** for the agent port (default: `7761`)

Build all binaries:

```bash
cargo build --release
```

Binaries will be in target/release/

# Agent & Controller CLI Summary
```
sai3bench-agent
USAGE:
  sai3bench-agent [--listen <addr>] [--tls] [--tls-domain <name>]
                [--tls-sans <csv>] [--tls-write-ca <dir>]

FLAGS/OPTIONS:
  --listen <addr>       Listen address (default: 0.0.0.0:7761)
  --tls                 Enable TLS with an ephemeral self-signed cert
  --tls-domain <name>   Subject CN / default SAN if --tls-sans not set (default: "localhost")
  --tls-sans <csv>      Comma-separated SANs (DNS names and/or IPs) for the cert (e.g. "hostA,10.1.2.3,127.0.0.1")
  --tls-write-ca <dir>  If set, writes PEM files (agent_cert.pem, agent_key.pem) into this directory
```

```
sai3bench-ctl
USAGE:
  sai3bench-ctl [--insecure] [--agent-ca <path>] [--agent-domain <name>] --agents <csv> <SUBCOMMAND> ...

GLOBAL FLAGS/OPTIONS:
  --agents <csv>        Comma-separated list of agent addresses (host:port)
  --insecure            Use plaintext (no TLS). Must match agent mode.
  --agent-ca <path>     Path to agent's certificate PEM (for TLS)
  --agent-domain <name> Override SNI / DNS name when validating TLS

SUBCOMMANDS:
  ping                              Ping agents and print versions
  get   --uri <s3://bucket/prefix>  Run GET workload via agents
         [--jobs <N>]

  put   --bucket <bucket> --prefix <prefix>
        [--object-size <bytes>] [--objects <count>] [--concurrency <N>]
```

**Note:** When agents are started with --tls, the controller must not
use --insecure. Instead, pass --agent-ca <path> to trust the agent’s
self‑signed certificate. 

If the agent cert doesn’t include the default DNS
name the controller uses, add --agent-domain.

# 2 Quick Start — Single Host (PLAINTEXT)
In one terminal:

## Run agent without TLS on port 7761
./target/release/sai3bench-agent --listen 127.0.0.1:7761
In another terminal:

## Controller talking to that agent, explicit plaintext:
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761 ping

## Example GET workload (jobs = concurrency for downloads)
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761 get \
  --uri s3://my-bucket/path/ --jobs 8

# 3 Multi-Host (PLAINTEXT)
On each agent host (e.g., node1, node2):

./sai3bench-agent --listen 0.0.0.0:7761
From the controller host:

./sai3bench-ctl --insecure --agents node1:7761,node2:7761 ping

./sai3bench-ctl --insecure --agents node1:7761,node2:7761 get \
  --uri s3://my-bucket/data/ --jobs 16

# 4 TLS with Self‑Signed Certificates (No CA hassles)
You can enable TLS on the agent with an ephemeral self‑signed certificate
generated at startup. You do not need a public CA. The controller just needs
the generated cert to trust the agent connection.

## 4.1 Start the Agent with TLS and write the cert
Pick a DNS name (CN) you’ll use from the controller—typically the agent’s
resolvable hostname or IP. If you need multiple names or IPs, use --tls-sans.

### Example: agent runs on loki-node3, reachable by name and IP
Write cert & key into /tmp/agent-ca/  (for you to scp to controller)
./sai3bench-agent \
  --listen 0.0.0.0:7761 \
  --tls \
  --tls-domain loki-node3 \
  --tls-sans "loki-node3,127.0.0.1,10.10.0.23" \
  --tls-write-ca /tmp/agent-ca

This produces:
   /tmp/agent-ca/agent_cert.pem
   /tmp/agent-ca/agent_key.pem


**Tip:** --tls-domain is the CN; if --tls-sans is not specified,
it will be used as a single SAN. If --tls-sans is provided, the SANs
list replaces the default and should include all names (or IPs) you plan to
use to connect to this agent.

Copy the certificate to the controller host (key stays on the agent):

### From controller host:
scp user@loki-node3:/tmp/agent-ca/agent_cert.pem /tmp/agent_ca.pem

## 4.2 Connect from the Controller (TLS)
Single agent:

```
./sai3bench-ctl \
  --agents loki-node3:7761 \
  --agent-ca /tmp/agent_ca.pem \
  ping
```

If you connect by an alternate name or IP that’s in the SANs, you may need
--agent-domain to set the SNI / TLS server_name to match the certificate:

## Connecting to the agent by IP, telling TLS to expect "loki-node3" (in SANs)
```
./sai3bench-ctl \
  --agents 10.10.0.23:7761 \
  --agent-ca /tmp/agent_ca.pem \
  --agent-domain loki-node3 \
  ping
```

Multiple agents (all in TLS mode):

```
./sai3bench-ctl \
  --agents loki-node3:7761,loki-node4:7761 \
  --agent-ca /tmp/agent_ca.pem \
  ping
```
```
./sai3bench-ctl \
  --agents loki-node3:7761,loki-node4:7761 \
  --agent-ca /tmp/agent_ca.pem \
  get --uri s3://my-bucket/data/ --jobs 16
```

**Important:** Do not pass --insecure to the controller when the agent is running with --tls.

# 5 Examples for Workloads
GET (download) via controller
### PLAINTEXT
```
./sai3bench-ctl --insecure --agents node1:7761 get \
  --uri s3://my-bucket/prefix/ --jobs 16
```

### TLS
```
./sai3bench-ctl --agents node1:7761 \
  --agent-ca /tmp/agent_ca.pem \
  get --uri s3://my-bucket/prefix/ --jobs 16
```
--uri accepts a single object (s3://bucket/key), a prefix (s3://bucket/prefix/), or a simple glob under a prefix (e.g., s3://bucket/prefix/*).

--jobs controls per-agent concurrency for GET.
PUT (upload) via controller

## Create N objects of size S under bucket/prefix, using M concurrency per agent.

### PLAINTEXT
```
./sai3bench-ctl --insecure --agents node1:7761 put \
  --bucket my-bucket \
  --prefix test/ \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 8
```

### TLS
```
./sai3bench-ctl --agents node1:7761 \
  --agent-ca /tmp/agent_ca.pem \
  put --bucket my-bucket \
  --prefix test/ \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 8
```

# 6 Localhost Demo (No Makefile Needed)
### Terminal A — agent (PLAINTEXT)
```
./target/release/sai3bench-agent --listen 127.0.0.1:7761
```

### Terminal B — controller
```
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761 ping
```

```
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761 get \
  --uri s3://my-bucket/prefix/ --jobs 4
```

## For TLS on localhost

### Terminal A — agent with TLS & SANs covering "localhost" and "127.0.0.1"
```
./sai3bench-agent --listen 127.0.0.1:7761 --tls \
  --tls-domain localhost \
  --tls-sans "localhost,127.0.0.1" \
  --tls-write-ca /tmp/agent-ca
```


### Terminal B — controller
```
./sai3bench-ctl --agents 127.0.0.1:7761 \
  --agent-ca /tmp/agent-ca/agent_cert.pem \
  --agent-domain localhost \
  ping
```

# 7 Troubleshooting
TLS is enabled ... but --agent-ca was not provided
You’re connecting to a TLS-enabled agent, but the controller is missing
--agent-ca. Provide the agent’s agent_cert.pem or run the controller with
--insecure (plaintext) if the agent is also plaintext.
h2 protocol error: http2 error / frame with invalid size

Most commonly a TLS name mismatch or wrong certificate. Ensure:
The controller uses --agent-ca that matches the agent’s certificate.
The SNI matches (--agent-domain) a SAN entry on the agent certificate.
The address you dial (host/IP) is present in the SANs (or you provide
--agent-domain to override the SNI to a SAN value).

No objects found for GET
Verify the --uri prefix and that your AWS credentials (on the agent
hosts) allow ListObjectsV2 and GetObject.

Throughput lower than expected
Increase --jobs (GET) or --concurrency (PUT), and/or add more agents.
Verify network path and S3 region distance.
Check CPU/network utilization on agent hosts.

# 8 Notes and Best Practices
Use resolvable hostnames for agents and include them in --tls-sans when
using TLS. If connecting by IP from the controller, add that IP to
--tls-sans or set --agent-domain to a SAN value.
Keep the private key (agent_key.pem) on the agent host; only the cert
(agent_cert.pem) should be copied to the controller(s).
For repeatable test environments, you can pre-generate and persist the certs
(via --tls-write-ca) and reuse them.

# 9 Running Tests
Unit + integration tests:

cargo test
The gRPC integration test starts a local agent, then checks controller
connectivity (plaintext). For full TLS tests between hosts, use the examples in
Sections 3–4.

# 10 Versioning
The agent reports its version on ping:

```
./sai3bench-ctl --insecure --agents node1:7761 ping
# connected to node1:7761 (agent version X.Y.Z)
```
Keep controller/agent binaries from the same source build when testing.
