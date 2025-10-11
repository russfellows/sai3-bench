# Documentation Directory

## User Documentation

### Getting Started
- **[USAGE.md](USAGE.md)** - Primary user guide for single-node and distributed modes
- **[DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host load generation guide
- **[CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md)** - S3, Azure, and GCS authentication guides

### Reference Documentation
- **[CHANGELOG.md](CHANGELOG.md)** - Complete version history (v0.1.0 → v0.6.4)
- **[CONFIG_SYNTAX.md](CONFIG_SYNTAX.md)** - YAML configuration file reference
- **[WARP_PARITY_STATUS.md](WARP_PARITY_STATUS.md)** - Warp/warp-replay compatibility status

## Development Documentation

### Implementation Summaries
- **[V0.6.4_MULTIHOST_SUMMARY.md](V0.6.4_MULTIHOST_SUMMARY.md)** - v0.6.4 results directory implementation
- **[V0.6.4_TESTING_SUMMARY.md](V0.6.4_TESTING_SUMMARY.md)** - v0.6.4 HDR histogram merging tests

## Maintenance

**Last Cleanup**: October 11, 2025 (v0.6.4)
- Removed 15 obsolete files (old release notes, completed plans, and outdated design docs)
- Consolidated version history into CHANGELOG.md
- Added DISTRIBUTED_TESTING_GUIDE.md for multi-host testing
- Replaced AZURE_SETUP.md with comprehensive CLOUD_STORAGE_SETUP.md (S3 + Azure + GCS)
- Removed s3dlio v0.9.4 migration docs (info now in CHANGELOG)
- Kept v0.6.4 implementation summaries for current release context

**Documentation Structure**:
- **9 total files** (down from 24)
- **User-facing docs**: 6 files for end users
- **Development docs**: 2 files for implementation reference
- **Index**: This README

**Cleanup Policy**:
- All release information goes into CHANGELOG.md (single source of truth)
- Implementation summaries kept for 1-2 major versions as reference
- Completed design documents removed once features are stable
- User guides and technical references kept permanently
