# YAML Config Converter: Legacy to Explicit Stages

## Overview

Starting in v0.8.61, sai3-bench requires all configurations to use **explicit stage definitions** instead of the old implicit-stage model. The config converter automates the migration from old configs to the new format.

## What Changed in v0.8.61

### Old Format (Implicit Stages)
```yaml
duration: 30s
concurrency: 32
target: "s3://bucket/test/"
workload:
  - op: get
    path: "data/*"
    weight: 100
```

**Implicit behavior**: Automatically runs stages in order:
1. Preflight validation (hidden)
2. Prepare (if prepare section exists)
3. Execute workload for `duration`
4. Cleanup (if prepare.cleanup=true)

### New Format (Explicit Stages)
```yaml
duration: 30s
concurrency: 32
target: "s3://bucket/test/"
workload:
  - op: get
    path: "data/*"
    weight: 100
distributed:
  agents: []  # Empty for standalone configs
  stages:
    - name: preflight
      order: 1
      type: validation
      completion: validation_passed
      timeout_secs: 300
    - name: execute
      order: 2
      type: execute
      completion: duration
      duration: 30s
```

**Explicit behavior**: Stages are defined explicitly with:
- **name**: Stage identifier
- **order**: Execution sequence (1, 2, 3, ...)
- **type**: Stage type (validation, prepare, execute, cleanup)
- **completion**: How stage knows it's done (validation_passed, tasks_done, duration)
- **duration**: For execute stages only

## Why Explicit Stages?

1. **Flexibility**: Run stages in any order (cleanup-first, multiple prepares, etc.)
2. **Clarity**: See exactly what will execute and when
3. **Advanced Features**: Custom stages, hybrid completion, optional stages
4. **Consistency**: Same format for standalone and distributed configs

## Using the Converter

### Basic Usage

```bash
# Dry-run (recommended first step)
./target/release/sai3-bench convert --config mixed.yaml --dry-run

# Convert single file
./target/release/sai3-bench convert --config mixed.yaml

# Convert multiple files
./target/release/sai3-bench convert --files tests/configs/*.yaml

# Skip validation (faster but risky)
./target/release/sai3-bench convert --config mixed.yaml --no-validate
```

### Batch Conversion

```bash
# Convert all old-style configs in tests/configs/
cd /home/eval/Documents/Code/sai3-bench
./target/release/sai3-bench convert --files tests/configs/*.yaml --dry-run

# Review which files will be converted, then run without --dry-run
./target/release/sai3-bench convert --files tests/configs/*.yaml
```

### Safety Features

1. **Automatic Backup**: Original files saved as `.yaml.bak`
2. **Dry-run Mode**: Preview changes without modifying files
3. **Validation**: Converted configs validated before replacement
4. **Skip Detection**: Files with existing stages automatically skipped

## Conversion Rules

### Stage Generation

The converter creates up to 4 stages:

1. **Preflight (always)**
   - Type: validation
   - Order: 1
   - Timeout: 300s
   - Validates config before execution

2. **Prepare (if prepare section exists)**
   - Type: prepare
   - Order: 2
   - Completion: tasks_done
   - Creates/verifies test objects

3. **Execute (always)**
   - Type: execute
   - Order: 3 (or 2 if no prepare)
   - Completion: duration
   - Duration: Taken from top-level `duration` field

4. **Cleanup (if prepare.cleanup=true)**
   - Type: cleanup
   - Order: Last
   - Completion: tasks_done
   - Deletes prepared objects

### Standalone vs Distributed

| Config Type | Old Format | New Format |
|-------------|------------|------------|
| **Standalone** | No `distributed` section | Creates minimal `distributed` with empty `agents[]` |
| **Distributed** | Has `distributed` but no `stages` | Adds `stages` to existing `distributed` section |

Both formats work identically - stages are required even for single-node execution.

## Examples

### Example 1: Simple GET Workload

**Before**:
```yaml
duration: 10s
concurrency: 8
target: "file:///tmp/test/"
workload:
  - op: get
    path: "data/*"
    weight: 100
```

**After**:
```yaml
duration: 10s
concurrency: 8
target: "file:///tmp/test/"
workload:
  - op: get
    path: "data/*"
    weight: 100
distributed:
  agents: []
  stages:
    - name: preflight
      order: 1
      type: validation
      completion: validation_passed
      timeout_secs: 300
    - name: execute
      order: 2
      type: execute
      completion: duration
      duration: 10s
```

### Example 2: With Prepare and Cleanup

**Before**:
```yaml
duration: 60s
concurrency: 16
target: "s3://bucket/test/"
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/test/data/"
      count: 1000
      size_spec: 1048576
  cleanup: true
workload:
  - op: get
    path: "data/*"
    weight: 100
```

**After**:
```yaml
# ... (full config preserved) ...
distributed:
  agents: []
  stages:
    - name: preflight
      order: 1
      type: validation
      completion: validation_passed
      timeout_secs: 300
    - name: prepare
      order: 2
      type: prepare
      completion: tasks_done
      expected_objects: 1000
    - name: execute
      order: 3
      type: execute
      completion: duration
      duration: 60s
    - name: cleanup
      order: 4
      type: cleanup
      completion: tasks_done
      expected_objects: 1000
```

## Testing Converted Configs

Always validate converted configs before running:

```bash
# 1. Convert with dry-run to preview
./target/release/sai3-bench convert --config myconfig.yaml --dry-run

# 2. Convert and create backup
./target/release/sai3-bench convert --config myconfig.yaml

# 3. Validate converted config
./target/release/sai3-bench run --config myconfig.yaml --dry-run

# 4. Run workload (if validation passes)
./target/release/sai3-bench run --config myconfig.yaml
```

## Rollback

If conversion fails or produces unexpected results:

```bash
# Restore from backup
mv myconfig.yaml.bak myconfig.yaml
```

## Advanced: Manual Conversion

If you need custom stage ordering or hybrid completion:

1. Let converter create base structure
2. Manually edit `distributed.stages` section
3. Validate with `--dry-run`

Example custom ordering:
```yaml
distributed:
  stages:
    - name: cleanup-old
      order: 1
      type: cleanup  # Cleanup BEFORE prepare!
      completion: tasks_done
    - name: preflight
      order: 2
      type: validation
      completion: validation_passed
    - name: prepare-epoch1
      order: 3
      type: prepare
      completion: tasks_done
    - name: execute-epoch1
      order: 4
      type: execute
      duration: 30s
      completion: duration
```

## Troubleshooting

### "No distributed/workload section - skipping"
**Cause**: Config has no `workload` section (invalid config)
**Fix**: Add a valid workload section or this is not an executable config

### "Already has stages section - skipping"
**Cause**: Config already converted to new format
**Fix**: No action needed - file is already in new format

### "Validation failed"
**Cause**: Converted config has syntax errors or invalid fields
**Fix**: Check error message, restore from `.bak`, fix manually

### Converted file is very verbose
**Cause**: serde_yaml serializes all fields including defaults
**Fix**: Functionally correct but verbose; manually clean up if needed

## Migration Strategy

For large projects with many configs:

1. **Inventory**: Find all YAML configs
   ```bash
   find . -name "*.yaml" -type f
   ```

2. **Test Batch**: Convert a few representative configs
   ```bash
   ./target/release/sai3-bench convert --files test1.yaml test2.yaml --dry-run
   ```

3. **Validate**: Test converted configs work
   ```bash
   ./target/release/sai3-bench run --config test1.yaml --dry-run
   ```

4. **Convert All**: Batch convert all configs
   ```bash
   ./target/release/sai3-bench convert --files *.yaml
   ```

5. **Commit Backups**: Keep `.bak` files in version control temporarily

6. **Test Suite**: Run full test suite with new configs

7. **Remove Backups**: After confirming everything works
   ```bash
   rm *.yaml.bak
   ```

## See Also

- [YAML_DRIVEN_STAGE_ORCHESTRATION.md](./YAML_DRIVEN_STAGE_ORCHESTRATION.md) - Stage system details
- [CONFIG_SYNTAX.md](./CONFIG_SYNTAX.md) - Full config reference
- [USAGE.md](./USAGE.md) - General usage guide
