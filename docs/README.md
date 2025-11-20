# Documentation Directory

## User Documentation

### Getting Started
- **[USAGE.md](USAGE.md)** - Primary user guide for single-node and distributed modes
- **[DIRECTORY_TREE_GUIDE.md](DIRECTORY_TREE_GUIDE.md)** - Hierarchical filesystem testing with directory trees (v0.7.0+)
- **[FILESYSTEM_TESTING_GUIDE.md](FILESYSTEM_TESTING_GUIDE.md)** - Filesystem operations and nested path testing (v0.7.0+)
- **[DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host load generation, scale-out vs scale-up patterns
- **[CONTAINER_DEPLOYMENT_GUIDE.md](CONTAINER_DEPLOYMENT_GUIDE.md)** - Run distributed tests with pre-started containers across cloud VMs
- **[SSH_SETUP_GUIDE.md](SSH_SETUP_GUIDE.md)** - One-command SSH automation for distributed testing
- **[CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md)** - S3, Azure, and GCS authentication guides

### Reference Documentation
- **[CHANGELOG.md](CHANGELOG.md)** - Complete version history (v0.1.0 → v0.8.0)
- **[CONFIG_SYNTAX.md](CONFIG_SYNTAX.md)** - YAML configuration file reference
- **[DATA_GENERATION.md](DATA_GENERATION.md)** - Data generation patterns and storage efficiency testing
- **[STATE_MACHINES.md](STATE_MACHINES.md)** - Agent and controller state machine architecture (v0.8.0+)
- **[IO_RATE_CONTROL_GUIDE.md](IO_RATE_CONTROL_GUIDE.md)** - I/O rate limiting and arrival patterns

### Performance & Analysis
- **[SCALE_OUT_VS_SCALE_UP.md](SCALE_OUT_VS_SCALE_UP.md)** - Performance comparison, cost analysis, deployment strategies
- **[FILE_IO_PATTERNS_ANALYSIS.md](FILE_IO_PATTERNS_ANALYSIS.md)** - File I/O patterns and performance characteristics

## Planning & Design

- **[planning/](planning/)** - Future implementation plans and design documents (5 docs)
  - RDF_BENCH_FEATURE_COMPARISON.md - rdf-bench feature comparison and implementation priorities
  - V0.8.1_FUTURE_ENHANCEMENTS.md - Planned features for next release
  - BLOCK_IO_IMPLEMENTATION_PLAN.md - Block-level I/O future work
  - DIRECTORY_STRUCTURE_DESIGN.md - Directory tree design details
  - DIRECTORY_TREE_SHARED_FILESYSTEM_DESIGN.md - Shared filesystem coordination

## Archived Documentation

- **[archive/](archive/)** - Completed implementation details and historical releases (12 docs)
  - v0.8.0: STATE_MACHINE_DESIGN_v0.8.0_detailed.md, PRIORITY_FIXES_v0.8.0.md, V0.8.0_IMPLEMENTATION_PLAN.md
  - v0.7.6: DISTRIBUTED_LIVE_STATS_IMPLEMENTATION_v0.7.6.md
  - v0.7.1: RELEASE_v0.7.1.md
  - v0.7.0: RELEASE_v0.7.0.md
  - v0.6.10: V0.6.10_PERFORMANCE_ANALYSIS.md
  - v0.6.9: V0.6.9_RELEASE_SUMMARY.md
  - v0.6.4: V0.6.4_MULTIHOST_SUMMARY.md, V0.6.4_TESTING_SUMMARY.md
  - rdf-bench comparisons: RDF_BENCH_REFERENCE.md, rdf-bench-vs-sai3-bench-comparison.md
  - See [archive/README.md](archive/README.md) for details

## Maintenance

**Last Cleanup**: November 20, 2025 (v0.8.0)
- **Added**: STATE_MACHINES.md - Concise reference for agent/controller state machines
- **Reorganized**: Combined RDF_BENCH_REFERENCE.md and rdf-bench comparison into planning/RDF_BENCH_FEATURE_COMPARISON.md
- **Moved to planning/**: rdf-bench feature comparison (now serves as implementation planning reference)
- **Moved to archive/**: Original rdf-bench reference docs (replaced by combined planning doc)
- **Archived**: Moved completed v0.8.0 implementation docs to archive/ (4 docs)
- **Updated**: CHANGELOG.md with comprehensive v0.8.0 release notes (formal state machines, error handling, auto-reset)
- **Updated**: USAGE.md with v0.8.0 error handling, auto-reset, and verbosity level documentation

**Previous cleanup**: October 31, 2025 (v0.7.0) - Added directory tree and filesystem guides

**Documentation Structure**:
- **15 active docs** in main directory (user guides, references, performance analysis)
- **5 planning docs** for future features and designs
- **12 archived docs** preserving implementation history
- **Total**: 32 documentation files

**Cleanup Policy**:
- All release information goes into CHANGELOG.md (single source of truth)
- Planning documents in planning/ directory (not yet implemented features)
- Completed implementation details archived with version tags
- User guides and reference docs kept current across releases
- Archive directory preserves historical context and design decisions
