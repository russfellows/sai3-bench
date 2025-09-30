# Overview
The plan for this tool is to create a basic, S3 testing tool, similar to MinIO's warp.  However, this tool leverages the [s3dlio Rust library project](https://github.com/russfellows/s3dlio)

So, what is different, and / or better?  In other words, why do we need this project?

1. This uses the AWS, S3 Rust SDK (warp does not)
2. This supports replay of captured workloads (warp, does not)
3. Support for settable data dedupe and compression levels (again, no in warp)
4. Better output logging and result analysis tools (warp analyze is hiddeously slow and difficult to obtain data as desired)


## Initial Plan
The initial plan for this project is captured in a [Discussion item #2](https://github.com/russfellows/warp-test/discussions/2)

## Usage
There is a brief user guide in docs directory, file USAGE.md.
[View Usage Guide](docs/USAGE.md)

## Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with s3-bench
- **[Changelog](docs/CHANGELOG.md)** - Version history and release notes
- **[Integration Context](docs/INTEGRATION_CONTEXT.md)** - Technical integration details

## Current Status (v0.2.2)

### ObjectStore Migration Complete ✅
- **Stage 2 Complete**: Full ObjectStore trait implementation for all backends
- **Multi-Backend Support**: Native `file://`, `direct://`, and `s3://` URI operations
- **Comprehensive Logging**: tracing infrastructure with `-v/-vv` CLI options
- **Zero Warnings**: Clean compilation after proper code analysis and fixes
- **Performance Validated**: 25k+ ops/s on file backend with 1ms latency

### s3dlio v0.8.7 Integration ✅
- **Pinned Dependency**: s3dlio v0.8.7 (rev cd4ee2e) for stable API
- **ObjectStore Operations**: All workload operations use ObjectStore trait
- **AWS SDK Compatibility**: Resolved version conflicts using fork patch system
- **Legacy Support**: CLI utilities retain s3_utils for backward compatibility

### Architecture
- **Single-Node CLI**: `s3-bench` for immediate testing with multi-backend support
- **Distributed Execution**: `s3bench-agent` (gRPC server) + `s3bench-ctl` (controller)
- **Metrics Collection**: HDR histograms with 9 size buckets per operation type
- **Backend Detection**: Automatic URI scheme recognition and ObjectStore routing

### Features
- **Multi-Backend Workloads**: Mix file://, direct://, and s3:// operations in single config
- **Verbose Logging**: `-v` for operational info, `-vv` for detailed debug tracing
- **Pattern Matching**: Glob patterns (`*`) and directory listings supported across backends
- **Concurrent Execution**: Semaphore-controlled concurrency with configurable worker counts

### Technical Notes
**Stage 2 Migration Complete** - Successfully migrated all operations from direct AWS SDK calls to ObjectStore trait. Multi-backend file:// and direct:// URIs now fully operational alongside S3, with comprehensive logging and performance validation. Ready for production testing across all supported backends.


