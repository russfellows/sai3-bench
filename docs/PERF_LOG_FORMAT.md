# Performance Log (perf-log) Format Specification

## Overview

The perf-log feature captures time-series performance metrics at configurable intervals during workload execution. Unlike op-log (which records individual operations), perf-log provides aggregate metrics for analysis of performance over time.

**Version**: v0.8.17+ (31 columns)

## Features

- **Delta-based metrics**: Operations and bytes per interval (not cumulative)
- **Warmup period flagging**: `is_warmup` column marks pre-measurement data
- **Stage tracking**: Prepare, workload, cleanup phases identified
- **Complete latency metrics**: Mean, p50, p90, p99 for each operation type
- **CPU utilization**: System, user, and I/O wait percentages
- **Agent identification**: Worker/agent ID for distributed workloads
- **Optional compression**: Automatic zstd compression with `.zst` extension

## File Format

- **Format**: TSV (tab-separated values)
- **Encoding**: UTF-8
- **Compression**: Optional zstd (detected by `.zst` extension)
- **Header**: First line contains column names

## Column Specification

| # | Column Name | Type | Description |
|---|-------------|------|-------------|
| 1 | `agent_id` | string | Worker identifier (e.g., "agent-1", "standalone") |
| 2 | `timestamp_epoch_ms` | u64 | Unix timestamp in milliseconds |
| 3 | `elapsed_s` | f64 | Seconds since workload start |
| 4 | `stage` | string | Current phase: "prepare", "workload", "cleanup", "listing" |
| 5 | `get_ops` | u64 | GET operations in this interval |
| 6 | `get_bytes` | u64 | Bytes read in this interval |
| 7 | `get_iops` | f64 | GET operations per second |
| 8 | `get_mbps` | f64 | GET throughput (MiB/s) |
| 9 | `get_mean_us` | u64 | GET latency mean (microseconds) |
| 10 | `get_p50_us` | u64 | GET latency 50th percentile (microseconds) |
| 11 | `get_p90_us` | u64 | GET latency 90th percentile (microseconds) |
| 12 | `get_p99_us` | u64 | GET latency 99th percentile (microseconds) |
| 13 | `put_ops` | u64 | PUT operations in this interval |
| 14 | `put_bytes` | u64 | Bytes written in this interval |
| 15 | `put_iops` | f64 | PUT operations per second |
| 16 | `put_mbps` | f64 | PUT throughput (MiB/s) |
| 17 | `put_mean_us` | u64 | PUT latency mean (microseconds) |
| 18 | `put_p50_us` | u64 | PUT latency 50th percentile (microseconds) |
| 19 | `put_p90_us` | u64 | PUT latency 90th percentile (microseconds) |
| 20 | `put_p99_us` | u64 | PUT latency 99th percentile (microseconds) |
| 21 | `meta_ops` | u64 | META operations in this interval (LIST, STAT, DELETE) |
| 22 | `meta_iops` | f64 | META operations per second |
| 23 | `meta_mean_us` | u64 | META latency mean (microseconds) |
| 24 | `meta_p50_us` | u64 | META latency 50th percentile (microseconds) |
| 25 | `meta_p90_us` | u64 | META latency 90th percentile (microseconds) |
| 26 | `meta_p99_us` | u64 | META latency 99th percentile (microseconds) |
| 27 | `cpu_user_pct` | f64 | CPU user time percentage |
| 28 | `cpu_system_pct` | f64 | CPU system/kernel time percentage |
| 29 | `cpu_iowait_pct` | f64 | CPU I/O wait percentage |
| 30 | `errors` | u64 | Error count in this interval |
| 31 | `is_warmup` | 0/1 | 1 if within warmup period, 0 otherwise |

## Column Groups

### Identity (1 column)
- `agent_id`: Identifies the worker/agent generating this row

### Timing (3 columns)
- `timestamp_epoch_ms`: Absolute time for correlation with external logs
- `elapsed_s`: Relative time for plotting workload progression
- `stage`: Current execution phase

### GET Metrics (8 columns)
- Operations, bytes, IOPS, throughput, mean latency, and percentiles (p50, p90, p99) for read operations

### PUT Metrics (8 columns)
- Operations, bytes, IOPS, throughput, mean latency, and percentiles (p50, p90, p99) for write operations

### META Metrics (6 columns)
- Operations, IOPS, mean latency, and percentiles (p50, p90, p99) for metadata operations (LIST, STAT, DELETE)
- Note: META operations don't have bytes/mbps (metadata doesn't transfer data)

### CPU Metrics (3 columns)
- User, system, and I/O wait CPU utilization percentages
- Useful for identifying CPU-bound vs I/O-bound workloads

### Status (2 columns)
- `errors`: Track error rates over time
- `is_warmup`: Filter out warmup period from analysis

## Configuration

```yaml
# Enable perf-log with default 1-second interval
perf_log:
  path: "/path/to/perf_metrics.tsv"
  interval: "1s"

# Optional warmup period (data before this is marked is_warmup=1)
warmup_period: "10s"
```

## Example Output

```tsv
agent_id	timestamp_epoch_ms	elapsed_s	stage	get_ops	get_bytes	get_iops	get_mbps	get_mean_us	get_p50_us	get_p90_us	get_p99_us	put_ops	put_bytes	put_iops	put_mbps	put_mean_us	put_p50_us	put_p90_us	put_p99_us	meta_ops	meta_iops	meta_mean_us	meta_p50_us	meta_p90_us	meta_p99_us	cpu_user_pct	cpu_system_pct	cpu_iowait_pct	errors	is_warmup
standalone	1766470049067	1.000	Workload	5350	9586720768	5348.0	9139.22	2459	531	5667	25631	1073	88076132	1072.6	83.96	470	323	924	2557	2949	2947.9	356	112	986	2823	0.0	0.0	0.0	0	0
standalone	1766470050067	2.001	Workload	5930	10191196160	5926.0	9712.52	2355	520	5423	24719	1172	96202448	1171.2	91.68	478	319	939	2553	3365	3362.7	331	103	921	2409	39.8	45.9	0.0	0	0
```

## Statistical Validity Note (v0.8.17+)

**Important for Distributed Mode:**

In distributed mode with multiple agents, the aggregate `perf_log.tsv` percentiles (p50, p90, p99) are computed using **weighted averaging**, which is a mathematical approximation. Percentiles cannot be accurately averaged - they should be computed from merged HDR histograms.

**For statistically accurate percentile analysis:**
- ✅ Use **per-agent perf_log files** (`results/agents/{agent-id}/perf_log.tsv`) - Accurate, computed from local HDR histograms
- ✅ Use **final workload_results.tsv** - Accurate, uses HDR histogram merging across all agents

The aggregate perf_log is suitable for **monitoring during execution** and visualization, but final detailed analysis should use the above sources.

**Standalone mode**: All percentiles are accurate (single HDR histogram, no aggregation).

## Analysis Tips

### Filtering Warmup Data
```bash
# Remove warmup rows for analysis (column 31)
awk -F'\t' 'NR==1 || $31==0' perf_metrics.tsv > perf_metrics_no_warmup.tsv
```

### Plotting with gnuplot
```gnuplot
set datafile separator "\t"
set xlabel "Elapsed Time (s)"
set ylabel "IOPS"
plot "perf_metrics.tsv" using 3:7 with lines title "GET IOPS"
```

### Aggregating Multi-Agent Data
```bash
# Combine perf-logs from multiple agents
# Header from first file, data from all
head -1 agent-1/perf.tsv > combined.tsv
tail -n +2 -q agent-*/perf.tsv >> combined.tsv
```

## Related Documentation

- [Op-log Format](./OPLOG_SORTING_CLARIFICATION.md) - Individual operation logging
- [Config Syntax](./CONFIG_SYNTAX.md) - Full configuration reference
- [Distributed Testing Guide](./DISTRIBUTED_TESTING_GUIDE.md) - Multi-agent workloads
