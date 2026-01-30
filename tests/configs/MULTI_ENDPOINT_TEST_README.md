# Multi-Endpoint Distributed Test - Two-Phase Approach

This test validates per-agent endpoint assignment for network topology optimization.

## Test Scenario
- **4 storage endpoints** (simulated as local directories)
- **2 agents** with different endpoint assignments:
  - Agent 1: endpoints A & B (round_robin strategy)
  - Agent 2: endpoints C & D (least_connections strategy)

## Phase 1: Prepare Data

Run `multi_endpoint_prepare.yaml`:
```bash
./target/release/sai3bench-ctl run --config tests/configs/multi_endpoint_prepare.yaml
```

**Expected**:
- 100 files created, distributed across 4 endpoints
- Approximate distribution: 25 files per endpoint (varies by load balancing)
- Files created in `testdata/` subdirectory within each endpoint

## Phase 2: Replicate Data (Manual)

Simulate shared storage namespace by copying files:
```bash
cd /tmp/sai3-multi-ep-test
for src in endpoint-{a,b,c,d}; do
  for dst in endpoint-{a,b,c,d}; do
    if [ "$src" != "$dst" ]; then
      cp -n $src/testdata/* $dst/testdata/ 2>/dev/null || true
    fi
  done
done
```

**Verify**: Each endpoint should have all 100 files:
```bash
for ep in a b c d; do
  echo "endpoint-$ep: $(ls endpoint-$ep/testdata/ | wc -l) files"
done
```

## Phase 3: Run Workload

Run `multi_endpoint_workload.yaml`:
```bash
./target/release/sai3bench-ctl run --config tests/configs/multi_endpoint_workload.yaml
```

**Expected**:
- 15-second test with GET/PUT/LIST operations
- Each agent uses only its assigned 2 endpoints
- Per-endpoint statistics in TSV output
- round_robin vs least_connections strategy differences visible

## Cleanup

```bash
rm -rf /tmp/sai3-multi-ep-test
```

## Key Configuration Points

1. **NO `target` field** in distributed multi-endpoint mode
2. **Prepare `base_uri`**: Path component only (`"testdata/"`)
3. **Workload paths**: Relative paths (`"testdata/*"`)
4. **`use_multi_endpoint: true`**: Required on prepare specs and workload ops
5. **Global `multi_endpoint`**: Required as fallback even with per-agent configs
