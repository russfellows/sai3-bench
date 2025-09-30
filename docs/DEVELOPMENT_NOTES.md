# Development Notes - io-bench v0.3.0

## Key Technical Discoveries & Solutions

This document captures the most important technical insights from the v0.3.0 multi-backend transformation.

### Azure Blob Storage URI Format Discovery

**Critical Finding**: Azure URIs must include the storage account name in the path.

```
✅ Correct:   az://STORAGE_ACCOUNT/CONTAINER/path
❌ Incorrect: az://CONTAINER/path (causes timeout/hangs)
```

**Root Cause**: s3dlio interprets the first path component as the storage account name. Using just the container name causes connections to `https://CONTAINER.blob.core.windows.net/` instead of the correct `https://STORAGE_ACCOUNT.blob.core.windows.net/`.

**Resolution Method**: Used s3dlio CLI (`../s3dlio/target/release/s3-cli`) for debugging, which revealed the URI format issue through verbose logging.

### Direct I/O Glob Pattern Bug Fix

**Issue**: Workload GET operations with glob patterns failed for `direct://` backend with "No URIs found".

**Root Cause**: s3dlio's ObjectStore returns normalized `file://` URIs for `direct://` operations, causing glob pattern matching to fail when comparing `direct://` patterns against `file://` results.

**Solution**: Added URI scheme normalization in `prefetch_uris_multi_backend()`:

```rust
fn normalize_scheme_for_matching(uri: &str) -> String {
    if let Some(scheme_end) = uri.find("://") {
        let path_part = &uri[scheme_end + 3..];
        format!("file://{}", path_part)
    } else {
        format!("file://{}", uri)
    }
}
```

### Dependency Chain Analysis

**Key Finding**: The `aws-smithy-http-client` patch is required by s3dlio upstream, not by io-bench directly.

**Dependency Chain**: 
```
io-bench → s3dlio → aws-smithy-http-client (patched)
```

**Implication**: Cannot remove the patch dependency until s3dlio upstream resolves their private API usage. This is documented for future maintenance.

### Performance Characteristics Validated

**File Backend**: 25k+ ops/s, sub-millisecond latencies - excellent for development/testing
**Direct I/O Backend**: 10+ MB/s throughput, ~100ms latencies - optimal for local performance testing
**Azure Blob Storage**: 2-3 ops/s, ~700ms latencies - network-dependent, suitable for cloud validation

### ObjectStore Migration Strategy

**Approach**: Gradual migration from legacy s3_utils to ObjectStore trait while maintaining backward compatibility.

**Key Insight**: Complete migration enables consistent behavior across all backends and simplifies maintenance.

### Authentication Patterns

**Azure Requirements**:
```bash
export AZURE_STORAGE_ACCOUNT="storage-account-name"
export AZURE_STORAGE_ACCOUNT_KEY="$(az storage account keys list --account-name ACCOUNT --query [0].value -o tsv)"
```

**AWS/S3**: Uses standard AWS SDK credential chain (environment variables, ~/.aws/credentials, etc.)

**File/Direct**: No authentication required

## Development Process Insights

### Systematic Backend Validation Approach

1. **CLI Testing**: Individual command validation (health, list, stat, get, put, delete)
2. **Workload Testing**: Configuration-driven mixed operations
3. **Error Scenario Testing**: Authentication failures, invalid URIs, network issues
4. **Performance Validation**: Throughput and latency measurements

### Debugging Techniques

- **s3dlio CLI**: Using upstream CLI tool to isolate library vs integration issues
- **Verbose Logging**: `-vv` flag for detailed operation tracing
- **Network Analysis**: Timeout patterns to identify connection issues
- **URI Parsing**: Systematic testing of URI format variations

### Testing Strategy

- **Local First**: File and Direct I/O backends for rapid iteration
- **Cloud Validation**: Real Azure and S3 operations for production readiness
- **Cross-Backend**: Mixed workloads to ensure consistent behavior

## Lessons Learned

1. **URI Format Validation**: Always test with actual cloud services, not just local emulation
2. **Upstream Dependencies**: Understanding dependency chains is crucial for maintenance
3. **Pattern Matching**: Cross-scheme operations require normalization strategies
4. **Documentation**: Real-world setup guides prevent user confusion
5. **Systematic Testing**: Methodical backend validation catches edge cases

## Future Considerations

### S3 Backend Completion
- Awaiting S3 access for full validation
- Expected to work similarly to Azure pattern

### Additional Backends
- Google Cloud Storage (GCS) - potential future addition
- Any s3dlio-supported backend can be integrated using established patterns

### Performance Optimization
- Backend-specific tuning opportunities identified
- Concurrent operation patterns validated across all backends

### Monitoring Integration
- HDR histogram metrics provide foundation for observability
- Microsecond precision enables detailed performance analysis