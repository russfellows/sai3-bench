# Credential Forwarding — sai3bench-ctl → agents (v0.8.92+)

## Overview

In a distributed run every agent host must have object-storage credentials
available before it can validate or run a workload.  Historically this meant
manually copying credentials to each machine — a tedious, error-prone process.

Starting with v0.8.92, `sai3bench-ctl` can **forward** credential environment
variables to all agents automatically.  The credentials travel inside the
existing gRPC control channel (embedded in the YAML config string) and are
applied to each agent's process environment before any storage operations begin.

---

## Quick Start

### Option A — Forward from the controller's own environment

If the controller already has credentials in its environment, they are forwarded
automatically with no additional flags:

```bash
# Set credentials on the controller host
export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY

# Run — credentials are forwarded to all agents automatically
sai3bench-ctl run --config workload.yaml
```

### Option B — Use a dedicated credentials file

A `.env`-format file can be passed with `--env-file`:

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

### Option C — Disable forwarding (agents self-configure)

```bash
sai3bench-ctl --no-forward-env run --config workload.yaml
```

Use this when agents already have credentials configured locally (e.g. via IAM
instance roles or Kubernetes secrets) and you do not want the controller to
interfere.

---

## Security Model

### Allow-list

Only these prefixes are ever forwarded.  All other variables are silently
ignored, even when present in an `--env-file`.

| Prefix / Key | Cloud / SDK |
|---|---|
| `AWS_*` | AWS SDK (access key, secret, session token, region, endpoint URL, …) |
| `GOOGLE_APPLICATION_CREDENTIALS` | GCP — service account JSON key path |
| `AZURE_STORAGE_*` | Azure Blob Storage (account name, key, SAS token, connection string) |

This is a **hard-coded allow-list** in the controller binary; it cannot be
extended without a code change.  Arbitrary environment variable forwarding is
intentionally not supported.

### Local environment wins

If a key already exists in the agent's own environment (e.g. an instance IAM
role sets `AWS_ROLE_ARN`), the forwarded value is **silently skipped**.
Local configuration always takes precedence over what the controller sends.

### Audit trail

The controller logs the **key names** (never values) it is forwarding:

```
🔑 Forwarding 2 credential variables from controller environment to all agents: AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
```

Each agent logs which keys it applied and which it skipped (because they were
already set locally):

```
INFO Applied 2 forwarded credential variables from controller: AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
```

### TLS warning

Credentials are embedded in the `config_yaml` field of the gRPC
`ControlMessage`.  When the connection is plaintext (no `--tls`), the
controller emits a warning:

```
⚠️  Credentials are being sent over an unencrypted (plaintext) gRPC connection.
     Enable --tls when operating over untrusted networks to protect credentials in transit.
```

Enable `--tls` and provide `--agent-ca` for production deployments on shared
networks.

### Never written to disk

The `distributed_env` field that carries credentials inside the YAML string is
serialized only **in memory**.  It uses `skip_serializing_if = "is_empty"` so
it never appears in:
- YAML config files loaded with `--config`
- Dry-run output (`run --dry-run`)
- Results saved to disk by the results writer

---

## How It Works Internally

```
Controller                                    Agent
──────────                                    ─────
1. sai3bench-ctl starts
2. Reads YAML config from disk
3. Calls load_credentials_for_agents()
   • --env-file: parse file with dotenvy
   • default: scan process environment
   • filter by allow-list
4. Sets config.distributed_env = {key: value, …}
5. Serializes config to YAML string
   (distributed_env is included because it is non-empty)
6. Sends config_yaml via gRPC PreFlightRequest
   ──────────────────────────────────────────►
                                              7. Parses YAML → Config struct
                                              8. Calls apply_distributed_env()
                                                 • For each key in distributed_env:
                                                   - If already in local env → skip
                                                   - Else → std::env::set_var(key, value)
                                              9. Runs pre-flight validation
                                                 (storage SDK now has credentials)
                                             10. Sends config_yaml via gRPC ControlMessage
                                                 for workload execution (same YAML)
                                             11. apply_distributed_env() called again
                                                 before workload starts
```

The same YAML string (with embedded credentials) is reused for both the
pre-flight validation RPC and the workload execution control message.

---

## `.env` File Format

Standard `key=value` format, one entry per line.  Comments (`# ...`) and blank
lines are supported.  Quote handling follows the `dotenvy` crate conventions
(same as the `dotenv` tool):

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

Non-credential lines (e.g. `HOME=/root` or `MY_APP_VAR=foo`) are silently
ignored — they are parsed but not included in the forwarded map.

---

## CLI Reference

```
sai3bench-ctl [OPTIONS] run --config <FILE>

Global credential-forwarding options:
  --env-file <FILE>     Path to a .env file whose credential variables are
                        forwarded to every agent.
                        Without this flag, the current process environment is
                        scanned for credential variables instead.

  --no-forward-env      Disable all credential forwarding.
                        Use when agents already have credentials configured
                        locally (IAM role, Kubernetes secret, etc.).

  --tls                 Enable TLS for gRPC connections (recommended when
                        forwarding credentials over non-local networks).
  --agent-ca <FILE>     PEM file for the agent's self-signed certificate (TLS).
```

---

## Configuration Validation

During `run --dry-run` (or pre-flight), the controller also checks whether the
configuration is likely to have credential issues at runtime and emits warnings
for:

- **No cloud credentials in controller environment** — when the YAML uses S3/GCS/
  Azure URIs with multiple agents and no `AWS_ACCESS_KEY_ID` /
  `GOOGLE_APPLICATION_CREDENTIALS` / `AZURE_STORAGE_ACCOUNT` is set in the
  controller's environment.  This is a reminder that without `--env-file` (or
  `--no-forward-env` with agents pre-configured), agents will fail to
  authenticate.

- **Redundant top-level `multi_endpoint`** — when every agent already has its own
  per-agent `multi_endpoint` block.  This pattern is confusing and was a common
  source of the pre-flight "64 errors" problem described in the v0.8.92 release
  notes.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `403 Forbidden` / `AccessDenied` during pre-flight | Credentials not forwarded or wrong | Check controller log for "Forwarding N credential variables"; verify key names |
| `No credentials detected` warning | Controller env has no matching keys and no `--env-file` | Set `AWS_ACCESS_KEY_ID` on controller or pass `--env-file` |
| Agent ignores forwarded credentials | Agent already has the same key set locally | Local env wins by design; unset the local key if you want the forwarded value |
| `Cannot read env file: …` error | `--env-file` path is wrong or not readable | Verify path and file permissions |
| Credentials appear in dry-run YAML output | Should never happen — file a bug | `distributed_env` field must never appear in serialized YAML |
