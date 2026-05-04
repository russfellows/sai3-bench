# sai3-bench Documentation

Comprehensive documentation for sai3-bench - a high-performance distributed I/O benchmarking tool.

---

## Quick Start

**New users start here:**

1. **[USAGE.md](USAGE.md)** - Complete user guide for workload configuration
2. **[DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host testing architecture and examples
3. **[DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md)** - SSH and container deployment options

---

## Core Documentation

### Essential Guides

| Document | Description |
|----------|-------------|
| **[USAGE.md](USAGE.md)** | Primary user guide - workload configuration, operations, examples |
| **[DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md)** | Multi-host architecture, per-agent endpoint assignment, verified examples |
| **[DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md)** | SSH setup, container deployment, production best practices |
| **[CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md)** | AWS, Azure, GCS authentication and configuration |

### Reference Documentation

| Document | Description |
|----------|-------------|
| **[CONFIG_SYNTAX.md](CONFIG_SYNTAX.md)** | YAML configuration file syntax reference |
| **[PERF_LOG_FORMAT.md](PERF_LOG_FORMAT.md)** | Performance log format (30-column TSV specification) |
| **[ANALYZE_TOOL.md](ANALYZE_TOOL.md)** | sai3-analyze tool - compare test runs in Excel |
| **[CONFIG_CONVERTER.md](CONFIG_CONVERTER.md)** | YAML config conversion tool - migrate and upgrade config files |
| **[S3DLIO_PERFORMANCE_TUNING.md](S3DLIO_PERFORMANCE_TUNING.md)** | s3dlio tuning guide (range downloads, concurrency, backends) |
| **[METADATA_CACHE_DESIGN.md](METADATA_CACHE_DESIGN.md)** | fjall LSM-tree metadata cache design and flush policy |
| **[CHANGELOG.md](CHANGELOG.md)** | Version history (v0.8.5+) |

---

## Key Features & Examples

### Multi-Endpoint Testing (v0.8.22+)

**Critical use case**: Different agents access different storage endpoints

```yaml
distributed:
  agents:
    # Agent 1: ONLY endpoints A & B
    - address: "host1:7167"
      id: "agent-dc-a"
      multi_endpoint:
        endpoints:
          - "s3://bucket/region-a/"
          - "s3://bucket/region-b/"
        strategy: round_robin
    
    # Agent 2: ONLY endpoints C & D
    - address: "host2:7167"
      id: "agent-dc-b"
      multi_endpoint:
        endpoints:
          - "s3://bucket/region-c/"
          - "s3://bucket/region-d/"
        strategy: least_connections
```

**Examples with verification**:

- [`tests/configs/multi_endpoint_prepare.yaml`](../tests/configs/multi_endpoint_prepare.yaml) - Data preparation with endpoint isolation
- [`tests/configs/multi_endpoint_workload.yaml`](../tests/configs/multi_endpoint_workload.yaml) - Workload test with per-agent stats

**See**: [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md#multi-endpoint-testing) for complete examples

### Distributed Testing

**Architecture**: Controller orchestrates multiple agents via gRPC

```text
Controller (sai3bench-ctl)
    ├─→ Agent 1 (host1:7167)
    ├─→ Agent 2 (host2:7167)
    └─→ Agent N (hostN:7167)
            ↓
    Storage Backend (S3/Azure/GCS/File)
```

**Quick start**:

```bash
# Start agents on each host
sai3bench-agent --listen 0.0.0.0:7167

# Run distributed test
sai3bench-ctl run --config distributed-test.yaml
```

**Examples**:

- [`tests/configs/local_test_2agents.yaml`](../tests/configs/local_test_2agents.yaml) - 2-agent local testing
- [`tests/configs/distributed_mixed_test.yaml`](../tests/configs/distributed_mixed_test.yaml) - Mixed operations

### Results Analysis

**sai3-analyze tool** - Compare multiple test runs in Excel:

```bash
sai3-analyze --dirs run1,run2,run3 --output comparison.xlsx
```

**Output includes**:

- Results tabs (aggregate metrics)
- Performance logs (time-series)
- Endpoint statistics (multi-endpoint tests)
- Timestamps formatted as readable dates

**See**: [ANALYZE_TOOL.md](ANALYZE_TOOL.md)

---

## Archived Documentation

Historical implementation details and earlier versions:

- **[archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md)** - Complete version history through v0.8.4
- **[archive/STATE_MACHINE_DESIGN_v0.8.0_detailed.md](archive/STATE_MACHINE_DESIGN_v0.8.0_detailed.md)** - State machine architecture
- **[archive/RDF_BENCH_REFERENCE.md](archive/RDF_BENCH_REFERENCE.md)** - rdf-bench comparison
- **[archive/](archive/)** - Design docs, planning documents, implementation notes

See [archive/README.md](archive/README.md) for complete list.

---

## Planning & Design Notes

Internal investigations, architecture decisions, and planning documents are in
[planning/](planning/) — not needed for day-to-day use:

- `trillion-object-throughput-investigation.md` — throughput investigation notes
- `How-to-Put-1-Trillion_Objects.md` — planning for trillion-object scale
- `BLOCK_STORAGE_ANALYSIS.md` — block storage feasibility analysis
- `FIX_HOT_PATH.md` — hot-path optimization sprint notes (shipped)
- `PrepareWorkloadArchitecture_v0.8.62.md` — prepare workload design (shipped)
- `YAML_DRIVEN_STAGE_ORCHESTRATION.md` — stage orchestration design (shipped)

---

## Version Information

**Current Version**: v0.8.97 (May 2026)

**Major features in v0.8.97**:

- **Namespace sharding** (`key_prefix_shards`): Distribute object keys across 256 hex prefix directories for hash-partitioned storage (VAST, etc.)
- **Dynamic PUT pool** (`dynamic_put_pool`): Newly PUT objects immediately enter the GET selection pool
- **Credential forwarding** (`sai3bench-ctl --env-file`): Automatic credential propagation to all agents (v0.8.92)
- **S3_ENDPOINT_URIS env var**: Enable multi-endpoint without editing YAML (v0.8.96)

**Recent major features**:

- v0.8.92: Credential forwarding (controller → agents via gRPC)
- v0.8.86: GCS RAPID (Hyperdisk ML) support
- v0.8.50: YAML-driven stage orchestration with 6 stage types and barrier sync
- v0.8.23: Pre-flight validation system
- v0.8.22: Multi-endpoint statistics, per-agent endpoint assignment

**See**: [CHANGELOG.md](CHANGELOG.md) for detailed release notes

---

## Getting Help

**Documentation not clear?** File an issue with:

- What you're trying to do
- What documentation you consulted
- Where the gap is

**Examples needed?** Check [`tests/configs/`](../tests/configs/) - all examples are verified with `--dry-run`

---

## Quick Reference

### Executables

```bash
sai3bench-agent -V    # v0.8.22 - Run on each test host
sai3bench-ctl -V      # v0.8.22 - Run on controller
sai3-analyze -V       # v0.8.22 - Analyze results (Excel)

**Last Cleanup**: November 24, 2025 (v0.8.5)
- **Added**: BIDIRECTIONAL_STREAMING.md - Consolidated architecture guide for v0.8.4+ bidirectional streaming
- **Added**: New CHANGELOG.md (v0.8.5+) - Archived old changelog (v0.1.0-v0.8.4) to archive/
- **Removed**: 7 detailed implementation docs (now obsolete/redundant, consolidated into BIDIRECTIONAL_STREAMING.md)
- **Simplified**: Reduced from 7 detailed implementation docs to 1 concise architecture guide
- **Status**: Bidirectional streaming fully implemented, tested, and production-ready

**Previous cleanup**: November 20, 2025 (v0.8.0) - State machine documentation reorganization

**Documentation Structure**:
- **16 active docs** in main directory (user guides, references, performance analysis)
- **5 planning docs** for future features and designs
- **13 archived docs** preserving implementation history (including CHANGELOG v0.1.0-v0.8.4)
- **Total**: 34 documentation files

**Cleanup Policy**:
- All release information goes into CHANGELOG.md (single source of truth)
- Planning documents in planning/ directory (not yet implemented features)
- Completed implementation details archived with version tags
- User guides and reference docs kept current across releases
- Archive directory preserves historical context and design decisions
