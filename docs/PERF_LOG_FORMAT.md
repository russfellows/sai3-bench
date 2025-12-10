# Performance Log (perf-log) Format Specification

## Overview

The perf-log feature captures time-series performance metrics at configurable intervals during workload execution. Unlike op-log (which records individual operations), perf-log provides aggregate metrics for analysis of performance over time.

**Version**: v0.8.15+

## Features

- **Delta-based metrics**: Operations and bytes per interval (not cumulative)
- **Warmup period flagging**: `is_warmup` column marks pre-measurement data
- **Stage tracking**: Prepare, workload, cleanup phases identified
- **Latency percentiles**: p50, p90, p99 for each operation type
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
| 1 | `agent_id` | string | Worker identifier (e.g., "agent-1", "local") |
| 2 | `timestamp_epoch_ms` | u64 | Unix timestamp in milliseconds |
| 3 | `elapsed_s` | f64 | Seconds since workload start |
| 4 | `stage` | string | Current phase: "prepare", "workload", "cleanup" |
| 5 | `get_ops` | u64 | GET operations in this interval |
| 6 | `get_bytes` | u64 | Bytes read in this interval |
| 7 | `get_iops` | f64 | GET operations per second |
| 8 | `get_mbps` | f64 | GET throughput (MiB/s) |
| 9 | `get_p50_us` | u64 | GET latency 50th percentile (microseconds) |
| 10 | `get_p90_us` | u64 | GET latency 90th percentile (microseconds) |
| 11 | `get_p99_us` | u64 | GET latency 99th percentile (microseconds) |
| 12 | `put_ops` | u64 | PUT operations in this interval |
| 13 | `put_bytes` | u64 | Bytes written in this interval |
| 14 | `put_iops` | f64 | PUT operations per second |
| 15 | `put_mbps` | f64 | PUT throughput (MiB/s) |
| 16 | `put_p50_us` | u64 | PUT latency 50th percentile (microseconds) |
| 17 | `put_p90_us` | u64 | PUT latency 90th percentile (microseconds) |
| 18 | `put_p99_us` | u64 | PUT latency 99th percentile (microseconds) |
| 19 | `meta_ops` | u64 | META operations in this interval (LIST, STAT, DELETE) |
| 20 | `meta_iops` | f64 | META operations per second |
| 21 | `meta_p50_us` | u64 | META latency 50th percentile (microseconds) |
| 22 | `meta_p90_us` | u64 | META latency 90th percentile (microseconds) |
| 23 | `meta_p99_us` | u64 | META latency 99th percentile (microseconds) |
| 24 | `cpu_user_pct` | f64 | CPU user time percentage |
| 25 | `cpu_system_pct` | f64 | CPU system/kernel time percentage |
| 26 | `cpu_iowait_pct` | f64 | CPU I/O wait percentage |
| 27 | `errors` | u64 | Error count in this interval |
| 28 | `is_warmup` | 0/1 | 1 if within warmup period, 0 otherwise |

## Column Groups

### Identity (1 column)
- `agent_id`: Identifies the worker/agent generating this row

### Timing (3 columns)
- `timestamp_epoch_ms`: Absolute time for correlation with external logs
- `elapsed_s`: Relative time for plotting workload progression
- `stage`: Current execution phase

### GET Metrics (7 columns)
- Operations, bytes, IOPS, throughput, and latency percentiles for read operations

### PUT Metrics (7 columns)
- Operations, bytes, IOPS, throughput, and latency percentiles for write operations

### META Metrics (5 columns)
- Operations, IOPS, and latency percentiles for metadata operations (LIST, STAT, DELETE)
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
agent_id	timestamp_epoch_ms	elapsed_s	stage	get_ops	get_bytes	get_iops	get_mbps	get_p50_us	get_p90_us	get_p99_us	put_ops	put_bytes	put_iops	put_mbps	put_p50_us	put_p90_us	put_p99_us	meta_ops	meta_iops	meta_p50_us	meta_p90_us	meta_p99_us	cpu_user_pct	cpu_system_pct	cpu_iowait_pct	errors	is_warmup
local	1733840400000	1.000	workload	1523	156237824	1523.0	149.02	425	890	2150	0	0	0.0	0.00	0	0	0	0	0.0	0	0	0	12.5	3.2	0.8	0	1
local	1733840401000	2.000	workload	1612	165347328	1612.0	157.70	410	875	2080	0	0	0.0	0.00	0	0	0	0	0.0	0	0	0	13.1	3.4	0.6	0	0
```

## Analysis Tips

### Filtering Warmup Data
```bash
# Remove warmup rows for analysis
awk -F'\t' 'NR==1 || $28==0' perf_metrics.tsv > perf_metrics_no_warmup.tsv
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
