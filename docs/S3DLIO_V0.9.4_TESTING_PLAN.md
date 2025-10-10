# s3dlio v0.9.4 Testing Plan

**Branch**: `s3dlio-v0.9.4-upgrade`  
**Version**: v0.6.1  
**Date**: October 9, 2025  
**Status**: Testing Phase

---

## Testing Objectives

1. Verify ObjectStore migration works across all backends
2. Confirm RangeEngine provides expected performance improvements
3. Validate backward compatibility with existing configs
4. Test distributed workload with migrated agent
5. Ensure no regressions in functionality

---

## Test Matrix

### Backend Coverage

| Backend | URI Scheme | Priority | Test Status |
|---------|------------|----------|-------------|
| **File** | `file://` | High | ⏳ Pending |
| **DirectIO** | `direct://` | Medium | ⏳ Pending |
| **S3** | `s3://` | High | ⏳ Pending |
| **Azure** | `az://` | High | ⏳ Pending |
| **GCS** | `gs://` | High | ⏳ Pending |

### Operation Coverage

| Operation | File | Direct | S3 | Azure | GCS |
|-----------|------|--------|----|----|-----|
| **GET** | ⏳ | ⏳ | ⏳ | ⏳ | ⏳ |
| **PUT** | ⏳ | ⏳ | ⏳ | ⏳ | ⏳ |
| **LIST** | ⏳ | ⏳ | ⏳ | ⏳ | ⏳ |
| **STAT** | ⏳ | ⏳ | ⏳ | ⏳ | ⏳ |
| **DELETE** | ⏳ | ⏳ | ⏳ | ⏳ | ⏳ |

### RangeEngine Coverage

| Test | File Size | Expected Behavior | Status |
|------|-----------|-------------------|--------|
| Below threshold | < 4 MB | Sequential download | ⏳ |
| At threshold | 4 MB | RangeEngine (1 range) | ⏳ |
| Medium file | 8-64 MB | RangeEngine (1-2 ranges) | ⏳ |
| Large file | 128 MB | RangeEngine (2 ranges) | ⏳ |
| Very large | 256+ MB | RangeEngine (4+ ranges) | ⏳ |

---

## Test Plans

### 1. File Backend Tests

**Priority**: High (baseline, no credentials needed)

#### Test 1.1: Basic Operations
```bash
# Create test data
./target/release/sai3-bench put \
  --uri file:///tmp/sai3test-v0.6.1/data/ \
  --objects 20 \
  --object-size 1048576 \
  --concurrency 4

# Test GET
./target/release/sai3-bench get \
  --uri "file:///tmp/sai3test-v0.6.1/data/*" \
  --jobs 4

# Test workload
./target/release/sai3-bench run \
  --config tests/configs/file_test.yaml
```

**Expected Results**:
- ✅ PUT: ~10-20k ops/s
- ✅ GET: ~20-30k ops/s
- ✅ Workload completes successfully
- ✅ No errors or warnings

#### Test 1.2: RangeEngine with Varying Sizes
```bash
./target/release/sai3-bench run \
  --config tests/configs/range_engine_test.yaml
```

**Expected Results**:
- ✅ Small files (< 4MB): Sequential (baseline performance)
- ✅ Large files (>= 4MB): May see slight improvement (limited for local I/O)
- ✅ No errors

#### Test 1.3: Distributed Workload
```bash
# Terminal 1: Start agent
./target/release/sai3bench-agent --listen 127.0.0.1:7761 -v &

# Terminal 2: Run distributed test
./target/release/sai3bench-ctl --insecure \
  --agents 127.0.0.1:7761 \
  run --config tests/configs/file_test.yaml
```

**Expected Results**:
- ✅ Agent connects successfully
- ✅ Workload executes on agent
- ✅ Results aggregated correctly
- ✅ Per-agent path isolation works

---

### 2. DirectIO Backend Tests

**Priority**: Medium (local, requires root or O_DIRECT support)

#### Test 2.1: Basic Operations
```bash
# Prepare mount point with O_DIRECT support
sudo mkdir -p /mnt/directio-test
# Mount with appropriate flags if needed

# Test PUT
./target/release/sai3-bench put \
  --uri direct:///mnt/directio-test/data/ \
  --objects 10 \
  --object-size 1048576 \
  --concurrency 2

# Test GET
./target/release/sai3-bench get \
  --uri "direct:///mnt/directio-test/data/*" \
  --jobs 2
```

**Expected Results**:
- ✅ PUT: Lower ops/s than file:// (alignment overhead)
- ✅ GET: Comparable or better than file:// (bypass page cache)
- ✅ No alignment errors
- ✅ RangeEngine threshold is 16MB (higher than file://)

**Notes**:
- DirectIO has higher overhead due to alignment requirements
- RangeEngine benefit is limited (already bypassing cache)
- May skip if O_DIRECT not available

---

### 3. S3 Backend Tests

**Priority**: High (production use case)

**Prerequisites**:
```bash
# Ensure AWS credentials are set
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
export AWS_DEFAULT_REGION="us-east-1"
# OR use .env file
```

#### Test 3.1: Basic Operations
```bash
# Update config with your bucket
export S3_TEST_BUCKET="s3://your-test-bucket/sai3bench-v0.6.1/"

# Test PUT
./target/release/sai3-bench put \
  --uri "${S3_TEST_BUCKET}data/" \
  --objects 10 \
  --object-size 1048576 \
  --concurrency 4

# Test GET
./target/release/sai3-bench get \
  --uri "${S3_TEST_BUCKET}data/*" \
  --jobs 4

# Test workload
./target/release/sai3-bench run \
  --config tests/configs/mixed.yaml
```

**Expected Results**:
- ✅ PUT: 100-500 ops/s (depends on bandwidth)
- ✅ GET: 100-1000 ops/s with RangeEngine
- ✅ RangeEngine activates for files >= 4MB
- ✅ Higher throughput than v0.6.0 for large files

#### Test 3.2: RangeEngine Performance
```bash
# Create varying sizes
./target/release/sai3-bench put \
  --uri "${S3_TEST_BUCKET}rangetest/4mb/" \
  --objects 5 --object-size 4194304 --concurrency 2

./target/release/sai3-bench put \
  --uri "${S3_TEST_BUCKET}rangetest/64mb/" \
  --objects 3 --object-size 67108864 --concurrency 2

./target/release/sai3-bench put \
  --uri "${S3_TEST_BUCKET}rangetest/128mb/" \
  --objects 2 --object-size 134217728 --concurrency 2

# Test GET performance
./target/release/sai3-bench get \
  --uri "${S3_TEST_BUCKET}rangetest/**" \
  --jobs 8 -vv
```

**Expected Results**:
- ✅ 4MB files: Baseline performance
- ✅ 64MB files: 20-30% faster than sequential
- ✅ 128MB files: 30-50% faster (2 concurrent ranges)
- ✅ Debug logs show RangeEngine activation

---

### 4. Azure Backend Tests

**Priority**: High (major RangeEngine beneficiary)

**Prerequisites**:
```bash
export AZURE_STORAGE_ACCOUNT="your-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-key"
# OR use .env file
```

#### Test 4.1: Basic Operations
```bash
export AZURE_TEST_URI="az://your-account/your-container/sai3bench-v0.6.1/"

# Test PUT
./target/release/sai3-bench put \
  --uri "${AZURE_TEST_URI}data/" \
  --objects 10 \
  --object-size 1048576 \
  --concurrency 4

# Test GET
./target/release/sai3-bench get \
  --uri "${AZURE_TEST_URI}data/*" \
  --jobs 4

# Test workload
./target/release/sai3-bench run \
  --config tests/configs/azure_test.yaml
```

**Expected Results**:
- ✅ PUT: 2-10 ops/s (Azure has higher latency)
- ✅ GET: 5-20 ops/s with RangeEngine
- ✅ No authentication errors
- ✅ Operations complete successfully

#### Test 4.2: RangeEngine Performance Comparison
```bash
# First update configs with your Azure details
# Edit: tests/configs/azure_rangeengine_test.yaml
# Edit: tests/configs/azure_rangeengine_disabled_test.yaml

# Test WITH RangeEngine (default)
./target/release/sai3-bench run \
  --config tests/configs/azure_rangeengine_test.yaml \
  -vv | tee azure_rangeengine_enabled.log

# Test WITHOUT RangeEngine (baseline)
./target/release/sai3-bench run \
  --config tests/configs/azure_rangeengine_disabled_test.yaml \
  -vv | tee azure_rangeengine_disabled.log

# Compare results
echo "=== RangeEngine Enabled ==="
grep "Total ops:" azure_rangeengine_enabled.log
echo "=== RangeEngine Disabled ==="
grep "Total ops:" azure_rangeengine_disabled.log
```

**Expected Results**:
- ✅ Enabled: 30-50% higher throughput for files >= 64MB
- ✅ Disabled: Baseline performance (slower)
- ✅ Small files (< 4MB): Similar performance (threshold not met)
- ✅ Debug logs show RangeEngine activity in enabled run

---

### 5. GCS Backend Tests

**Priority**: High (major RangeEngine beneficiary)

**Prerequisites**:
```bash
# Ensure GCP credentials are set
export GOOGLE_APPLICATION_CREDENTIALS="/path/to/service-account-key.json"
# OR use default application credentials
```

#### Test 5.1: Basic Operations
```bash
export GCS_TEST_URI="gs://your-bucket/sai3bench-v0.6.1/"

# Test PUT
./target/release/sai3-bench put \
  --uri "${GCS_TEST_URI}data/" \
  --objects 10 \
  --object-size 1048576 \
  --concurrency 4

# Test GET
./target/release/sai3-bench get \
  --uri "${GCS_TEST_URI}data/*" \
  --jobs 4

# Test workload (update gcs_test.yaml with your bucket)
./target/release/sai3-bench run \
  --config tests/configs/gcs_test.yaml
```

**Expected Results**:
- ✅ PUT: 10-100 ops/s (depends on location/bandwidth)
- ✅ GET: 20-200 ops/s with RangeEngine
- ✅ No authentication errors
- ✅ RangeEngine activates for files >= 4MB

#### Test 5.2: RangeEngine Performance
```bash
# Create test objects with varying sizes
./target/release/sai3-bench put \
  --uri "${GCS_TEST_URI}rangetest/8mb/" \
  --objects 5 --object-size 8388608 --concurrency 2

./target/release/sai3-bench put \
  --uri "${GCS_TEST_URI}rangetest/128mb/" \
  --objects 3 --object-size 134217728 --concurrency 2

# Test GET with debug logging
./target/release/sai3-bench get \
  --uri "${GCS_TEST_URI}rangetest/**" \
  --jobs 8 -vv
```

**Expected Results**:
- ✅ 8MB files: 20-30% improvement over sequential
- ✅ 128MB files: 30-50% improvement (2 ranges)
- ✅ Debug logs: "RangeEngine (GCS) downloaded X bytes in N ranges"
- ✅ INFO logs show throughput improvements

---

## Regression Testing

### Test Existing Configs
```bash
# Run all existing test configs to ensure backward compatibility
for config in tests/configs/*.yaml; do
  echo "Testing: $config"
  timeout 60 ./target/release/sai3-bench run --config "$config" || echo "FAILED: $config"
done
```

**Expected**: All existing configs work without modification

---

## Performance Benchmarks

### Baseline Comparison (v0.6.0 vs v0.6.1)

Create script: `tests/performance_comparison.sh`
```bash
#!/bin/bash
# Compare v0.6.0 (git checkout main) vs v0.6.1 (current branch)

# Test file backend (no RangeEngine benefit expected)
echo "=== File Backend (no RangeEngine benefit) ==="
./target/release/sai3-bench run --config tests/configs/file_test.yaml

# Test Azure backend (significant RangeEngine benefit expected)
echo "=== Azure Backend (RangeEngine benefit) ==="
./target/release/sai3-bench run --config tests/configs/azure_test.yaml
```

**Expected Metrics**:
- File backend: Similar performance (±5%)
- Azure/GCS/S3 backend: 30-50% faster GET ops for files >= 4MB

---

## Unit Testing

```bash
# Run all unit tests
cargo test --lib

# Run with verbose output
cargo test --lib -- --nocapture

# Run specific test
cargo test test_bucket_index
```

**Expected**: All 18 tests pass

---

## Integration Testing

```bash
# Run integration tests
cargo test --test '*'

# Specific integration test
cargo test --test grpc_integration
```

**Expected**: All integration tests pass

---

## Distributed Testing

### Multi-Agent Setup
```bash
# Terminal 1: Agent 1
./target/release/sai3bench-agent --listen 127.0.0.1:7761 -v

# Terminal 2: Agent 2
./target/release/sai3bench-agent --listen 127.0.0.1:7762 -v

# Terminal 3: Controller
./target/release/sai3bench-ctl --insecure \
  --agents 127.0.0.1:7761,127.0.0.1:7762 \
  run --config tests/configs/file_test.yaml
```

**Expected Results**:
- ✅ Both agents connect
- ✅ Workload distributed across agents
- ✅ Per-agent isolation works
- ✅ Results aggregated correctly
- ✅ No agent failures

---

## Stress Testing

### Long-Running Workload
```bash
# 5-minute sustained load
./target/release/sai3-bench run \
  --config tests/configs/file_test.yaml \
  --duration 300s
```

**Monitor**:
- Memory usage (should be stable)
- CPU usage (should be consistent)
- Operation latency (no degradation)
- No errors or panics

---

## Error Handling

### Test Error Cases
1. Invalid URI: `./target/release/sai3-bench get --uri "invalid://test"`
2. Missing credentials: Unset AWS/Azure/GCS env vars
3. Non-existent objects: `./target/release/sai3-bench get --uri "file:///nonexistent/*"`
4. Network failures: Disconnect during operation
5. Insufficient permissions: Read-only credentials with PUT

**Expected**: Graceful error messages, no panics

---

## Test Results Template

```markdown
## Test Session: [Date/Time]

### Environment
- OS: Linux [version]
- Rust: [rustc --version]
- sai3-bench: v0.6.1 (branch: s3dlio-v0.9.4-upgrade)
- s3dlio: v0.9.4

### Backend Tests

#### File Backend
- [ ] Basic operations: PASS/FAIL
- [ ] Workload test: PASS/FAIL
- [ ] Distributed test: PASS/FAIL
- Notes: ...

#### S3 Backend
- [ ] Basic operations: PASS/FAIL
- [ ] RangeEngine test: PASS/FAIL
- Performance: [ops/s]
- Notes: ...

#### Azure Backend
- [ ] Basic operations: PASS/FAIL
- [ ] RangeEngine enabled: PASS/FAIL
- [ ] RangeEngine disabled: PASS/FAIL
- Performance improvement: [%]
- Notes: ...

#### GCS Backend
- [ ] Basic operations: PASS/FAIL
- [ ] RangeEngine test: PASS/FAIL
- Performance: [ops/s]
- Notes: ...

### Regression Tests
- [ ] All existing configs: PASS/FAIL
- [ ] Unit tests: [N/18] passed
- [ ] Integration tests: PASS/FAIL

### Issues Found
1. [Description]
2. [Description]

### Conclusions
[Summary]
```

---

## Success Criteria

✅ **Minimum Requirements for Merge**:
1. All unit tests pass (18/18)
2. File backend tests pass completely
3. At least 2 network backends tested successfully (S3, Azure, or GCS)
4. Distributed workload tests pass
5. No regressions in existing functionality
6. RangeEngine shows measurable improvement on at least one backend
7. Clean compilation with zero warnings

✅ **Ideal State**:
1. All 5 backends tested (File, DirectIO, S3, Azure, GCS)
2. RangeEngine performance validated on Azure and GCS
3. Performance improvements documented
4. All edge cases handled gracefully

---

## Next Steps After Testing

1. Update CHANGELOG.md with test results
2. Update docs/S3DLIO_V0.9.4_MIGRATION.md with actual performance numbers
3. Create pull request to merge into main
4. Tag as v0.6.1 after merge
5. Update documentation with best practices

---

## Questions to Answer Through Testing

1. What is the actual RangeEngine performance improvement for:
   - S3 large files?
   - Azure large files?
   - GCS large files?

2. At what file size does RangeEngine benefit become significant?
   - Is 4MB threshold optimal?
   - Should we recommend different thresholds for different backends?

3. Are there any compatibility issues with:
   - Existing YAML configs?
   - Distributed workloads?
   - Different object sizes?

4. Do we need to adjust default RangeEngine settings for any backend?

5. Are there any edge cases or failure modes we didn't anticipate?

---

**Testing Status**: ⏳ Ready to begin  
**Updated**: October 9, 2025
