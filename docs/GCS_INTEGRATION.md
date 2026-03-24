# GCS / RAPID Integration — sai3-bench

**Status**: Fully supported and verified working  
**Since**: sai3-bench v0.8.86, s3dlio v0.9.84 or later

---

GCS standard storage and RAPID (Hyperdisk ML) storage both work correctly.
All GCS I/O implementation lives in the s3dlio library — sai3-bench simply
passes configuration through to s3dlio before the first `gs://` operation.

## Quick Start

Authentication uses Application Default Credentials (ADC):

```bash
gcloud auth application-default login
```

### Standard GCS workload

```yaml
target: "gs://my-bucket/bench/"
concurrency: 32
```

No additional configuration is required. The gRPC subchannel count defaults
to `concurrency` (one channel per task).

### GCS RAPID bucket

```yaml
target: "gs://my-rapid-bucket/bench/"
concurrency: 32

s3dlio_optimization:
  gcs_rapid_mode: true
  enable_range_downloads: false   # RAPID uses bidi streaming, not byte ranges
```

RAPID mode can also be auto-detected per bucket. Omit `gcs_rapid_mode` and
s3dlio will call `GetStorageLayout` on first access to determine the bucket
type (result cached for the process lifetime).

## RAPID vs Standard GCS

| | Standard | RAPID (Hyperdisk ML) |
|-|----------|---------------------|
| **PUT API** | `InsertObject` | `BidiWriteObject` |
| **GET API** | `ReadObject` | `BidiReadObject` |
| **Auto-detected** | — | Yes (via `GetStorageLayout`) |
| **Force via YAML** | `gcs_rapid_mode: false` | `gcs_rapid_mode: true` |

RAPID objects must be read back with the bidi-read API. s3dlio handles this
automatically.

## Optional: Override gRPC Channel Count

For very high concurrency or multi-host workloads, the channel count can be
tuned explicitly:

```yaml
concurrency: 32

s3dlio_optimization:
  gcs_channel_count: 4    # total gRPC subchannels = 4
  gcs_rapid_mode: true
  enable_range_downloads: false
```

When `gcs_channel_count` is absent (the usual case), sai3-bench sets one
channel per concurrent task automatically.

## Environment Variables

Prefer YAML fields over environment variables. Available overrides:

| Variable | Purpose |
|----------|---------|
| `GOOGLE_APPLICATION_CREDENTIALS` | Path to ADC JSON (if not using `gcloud auth`) |
| `S3DLIO_GCS_RAPID` | Force `true` / `false` / `auto` |
| `S3DLIO_GCS_GRPC_CHANNELS` | Override subchannel count |

## See Also

- [CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md) — authentication setup for all backends
- [S3DLIO_PERFORMANCE_TUNING.md](S3DLIO_PERFORMANCE_TUNING.md) — general s3dlio tuning (S3-focused)
