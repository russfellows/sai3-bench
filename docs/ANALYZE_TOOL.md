# sai3-analyze - Results Consolidation Tool

## Overview

`sai3-analyze` is a command-line tool that consolidates multiple sai3-bench results directories into a single Microsoft Excel spreadsheet. This solves the problem of Excel refusing to open multiple files with the same name (e.g., multiple `results.tsv` files).

## Features

- ✅ Consolidates multiple results directories into one Excel file
- ✅ Creates two tabs per results directory (results.tsv and prepare_results.tsv)
- ✅ Generates short, unique tab names (Excel 31-character limit compliant)
- ✅ Preserves all data including headers, numbers, and strings
- ✅ Auto-formats columns for readability
- ✅ Sorts results by timestamp for consistent ordering
- 🔷 Per-agent results support (planned for future release)

## Installation

The tool is built as part of the sai3-bench project:

```bash
cd sai3-bench
cargo build --release --bin sai3-analyze
```

The binary will be created at: `target/release/sai3-analyze`

## Usage

### Basic Usage - Pattern Matching

Analyze all results directories matching a pattern:

```bash
# Analyze all sai3-* directories in current directory
sai3-analyze --pattern "sai3-*" --output results.xlsx

# Analyze from a specific base directory
sai3-analyze --pattern "sai3-*" --base-dir ~/results/GCP-sai3_22-Dec-2025 --output gcp-results.xlsx
```

### Specific Directories

Analyze specific directories by listing them:

```bash
# Comma-separated list
sai3-analyze --dirs sai3-20251222-1744-sai3-resnet50_1hosts,sai3-20251222-1759-sai3-resnet50-getonly_1hosts --output comparison.xlsx

# With base directory
sai3-analyze \
  --base-dir ~/results \
  --dirs experiment1,experiment2,experiment3 \
  --output experiments.xlsx
```

## Tab Naming Convention

Tab names are automatically generated to be short, unique, and meaningful:

**Format**: `MMDD-HHMM-workload_Xh-R/P`

- `MMDD-HHMM`: Month-day and hour-minute from timestamp
- `workload`: Workload name (shortened, removes "sai3-" prefix)
- `Xh`: Number of hosts (e.g., "1h", "8h")
- `R` or `P`: Results.tsv or Prepare_results.tsv

**Examples**:

| Directory Name | Results Tab | Prepare Tab |
|----------------|-------------|-------------|
| `sai3-20251222-1744-sai3-resnet50_1hosts` | `1222-1744-resnet50_1h-R` | `1222-1744-resnet50_1h-P` |
| `sai3-20251222-1914-sai3-resnet50_8hosts` | `1222-1914-resnet50_8h-R` | `1222-1914-resnet50_8h-P` |
| `sai3-20251222-2129-sai3-unet3d_8hosts` | `1222-2129-unet3d_8h-R` | `1222-2129-unet3d_8h-P` |

Tab names are automatically truncated to 31 characters if needed (Excel limitation).

## Example Output

Given this directory structure:

```
GCP-sai3_22-Dec-2025/
├── sai3-20251222-1744-sai3-resnet50_1hosts/
│   ├── results.tsv
│   └── prepare_results.tsv
├── sai3-20251222-1914-sai3-resnet50_8hosts/
│   ├── results.tsv
│   └── prepare_results.tsv
└── sai3-20251222-2129-sai3-unet3d_8hosts/
    ├── results.tsv
    └── prepare_results.tsv
```

Running:
```bash
sai3-analyze --pattern "sai3-*" --base-dir GCP-sai3_22-Dec-2025 --output gcp-results.xlsx
```

Creates `gcp-results.xlsx` with 6 tabs:
1. `1222-1744-resnet50_1h-R` - Results from first run
2. `1222-1744-resnet50_1h-P` - Prepare results from first run
3. `1222-1914-resnet50_8h-R` - Results from second run
4. `1222-1914-resnet50_8h-P` - Prepare results from second run
5. `1222-2129-unet3d_8h-R` - Results from third run
6. `1222-2129-unet3d_8h-P` - Prepare results from third run

## Data Preservation

The tool preserves all data from TSV files:
- **Headers**: Bold formatting applied
- **Numbers**: Parsed and stored as numeric values (enables Excel calculations)
- **Strings**: Preserved as text
- **Column widths**: Auto-sized for readability

## Command-Line Options

| Option | Short | Description | Default |
|--------|-------|-------------|---------|
| `--pattern` | `-p` | Glob pattern for directory matching | None |
| `--dirs` | `-d` | Comma-separated list of specific directories | None |
| `--base-dir` | `-b` | Base directory to search in | `.` (current) |
| `--output` | `-o` | Output Excel file path | `sai3bench-results.xlsx` |
| `--include-agents` | | Include per-agent results (future) | Not yet implemented |

## Error Handling

The tool provides helpful error messages:

```bash
# No results found
$ sai3bench-analyze --pattern "nonexistent-*"
Error: No results directories found matching pattern in "."
Use --pattern 'sai3-*' or --dirs dir1,dir2,dir3

# Missing TSV files
$ sai3bench-analyze --dirs empty-directory
Warning: Directory not found or not a directory: "empty-directory"
Error: No TSV files found in any results directory

# Invalid directory
$ sai3bench-analyze --base-dir /nonexistent
Error: Failed to read TSV file: ...
```

## Future Enhancements

### Per-Agent Results (Planned)

Future versions will support including per-agent results:

```bash
sai3bench-analyze --pattern "sai3-*" --include-agents --output full-results.xlsx
```

This will add additional tabs for each agent:
- `1222-1744-resnet50_1h-agent1-R`
- `1222-1744-resnet50_1h-agent1-P`
- `1222-1744-resnet50_1h-agent2-R`
- `1222-1744-resnet50_1h-agent2-P`

### Additional Features Under Consideration

- Summary tab with overview statistics
- Comparison charts (throughput, latency percentiles)
- Filtering by workload type or host count
- CSV output option
- Custom tab naming templates

## Tips and Best Practices

1. **Organize Results**: Keep results directories in a dedicated folder for easier batch processing

2. **Meaningful Names**: Use descriptive workload names in your sai3-bench configs for better tab names

3. **Batch Analysis**: Run the tool after each experiment session to consolidate results immediately

4. **Excel Limitations**: Remember that Excel has a limit of 255 sheets per workbook

5. **Large Datasets**: For datasets with many rows, consider filtering in Excel or using pivot tables

## Troubleshooting

### "No results directories found"

- Check that you're in the correct base directory
- Verify the pattern matches your directory names
- Use `ls -la` to confirm directory structure

### "Tab name exceeds Excel limit"

- Tab names are automatically truncated to 31 characters
- If you see this warning, some information may be lost in the tab name
- The full directory information is still available in the data

### Excel can't open the file

- Ensure you have Microsoft Excel 2007 or later (for .xlsx format)
- Try opening with LibreOffice Calc as an alternative
- Check file permissions

## See Also

- [sai3-bench README](../README.md) - Main benchmarking tool documentation
- [Distributed Testing Guide](DISTRIBUTED_TESTING_GUIDE.md) - Running distributed benchmarks
- [Results Format](RESULTS_FORMAT.md) - Understanding TSV output format (if exists)
