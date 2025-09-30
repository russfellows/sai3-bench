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

## Current Status (v0.2.1)

### s3dlio v0.8.7 Integration ✅
- **Pinned Dependency**: s3dlio v0.8.7 (rev cd4ee2e) for stable API
- **Multi-Backend Foundation**: Prepared for file://, direct://, and s3:// URI support
- **Clean Compilation**: All binaries compile without warnings
- **AWS SDK Compatibility**: Resolved version conflicts using fork patch system

### Architecture
- **Single-Node CLI**: `s3-bench` for immediate testing
- **Distributed Execution**: `s3bench-agent` (gRPC server) + `s3bench-ctl` (controller)
- **Metrics Collection**: HDR histograms with 9 size buckets
- **Backend Detection**: Automatic URI scheme recognition

### Technical Notes
Currently in **Stage 1** completion - successfully integrated s3dlio v0.8.7 with compilation fixes and import structure updates. **Stage 2** (ObjectStore trait migration) will enable multi-backend operations for file:// and direct:// URIs while maintaining S3 performance optimizations.


