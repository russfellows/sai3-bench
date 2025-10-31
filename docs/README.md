# Documentation Directory

## User Documentation

### Getting Started
- **[USAGE.md](USAGE.md)** - Primary user guide for single-node and distributed modes
- **[DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host load generation, scale-out vs scale-up patterns
- **[CONTAINER_DEPLOYMENT_GUIDE.md](CONTAINER_DEPLOYMENT_GUIDE.md)** - Run distributed tests with pre-started containers across cloud VMs
- **[SSH_SETUP_GUIDE.md](SSH_SETUP_GUIDE.md)** - One-command SSH automation for distributed testing
- **[SCALE_OUT_VS_SCALE_UP.md](SCALE_OUT_VS_SCALE_UP.md)** - Performance comparison, cost analysis, deployment strategies
- **[CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md)** - S3, Azure, and GCS authentication guides

### Reference Documentation
- **[CHANGELOG.md](CHANGELOG.md)** - Complete version history (v0.1.0 → v0.6.11)
- **[CONFIG_SYNTAX.md](CONFIG_SYNTAX.md)** - YAML configuration file reference
- **[DATA_GENERATION.md](DATA_GENERATION.md)** - Data generation patterns and storage efficiency testing

## Technical Documentation

### Performance & Optimization
- **[IO_SIZE_OPTIMIZATION.md](IO_SIZE_OPTIMIZATION.md)** - I/O size tuning for different backends
- **[CHUNKED_READS_STRATEGY.md](CHUNKED_READS_STRATEGY.md)** - Direct I/O chunked read implementation
- **[CHUNKED_READS_IMPLEMENTATION.md](CHUNKED_READS_IMPLEMENTATION.md)** - Technical details of chunked read optimization

## Archived Documentation

- **[archive/](archive/)** - Version-specific release notes and implementation summaries
  - v0.6.4, v0.6.9, v0.6.10 detailed release documentation
  - See [archive/README.md](archive/README.md) for details

## Maintenance

**Last Cleanup**: October 22, 2025 (v0.6.11+)
- **Added**: CONTAINER_DEPLOYMENT_GUIDE.md for manual container deployment workflows
- **Archived**: 4 version-specific docs (v0.6.4, v0.6.9, v0.6.10) - functionality integrated into feature guides
- **Previous cleanup**: October 20, 2025 - Added SSH_SETUP_GUIDE.md, SCALE_OUT_VS_SCALE_UP.md for v0.6.11 distributed features
- **Updated**: DISTRIBUTED_TESTING_GUIDE.md with SSH automation, scale-up patterns, Podman support
- **Updated**: CHANGELOG.md with comprehensive v0.6.11 distributed testing release notes

**Documentation Structure**:
- **14 active files** (12 docs + 1 archive dir + this README)
- **User-facing**: 9 guides (getting started, distributed, container deployment, cloud setup)
- **Technical**: 3 optimization/implementation docs
- **Reference**: 1 changelog, 1 config syntax
- **Archived**: 4 version-specific docs (preserved for historical reference)

**Cleanup Policy**:
- All release information goes into CHANGELOG.md (single source of truth)
- Version-specific implementation docs archived after features are mature and integrated
- Feature guides remain current across releases (not tied to specific versions)
- User guides and technical references kept permanently
- Archive directory preserves historical context
