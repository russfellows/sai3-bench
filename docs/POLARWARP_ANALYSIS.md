# polarWarp Analysis - TSV Output Format Reference

## Overview
polarWarp is a Python+Polars tool that parses MinIO Warp output logs 37x faster than warp's built-in tools, producing superior output with size-bucketed metrics.

## Key Insights for io-bench v0.5.1

### Size Buckets (8 buckets vs our 9)
polarWarp uses **8 size buckets**:

| Bucket # | Label | Range |
|----------|-------|-------|
| 0 | None/NaN | 0 bytes (metadata ops) |
| 1 | 1 - 32k | 1B to 32KB |
| 2 | 32k - 128k | 32KB to 128KB |
| 3 | 128k - 1mb | 128KB to 1MB |
| 4 | 1m - 8mb | 1MB to 8MB |
| 5 | 8m - 64mb | 8MB to 64MB |
| 6 | 64m - 999mb | 64MB to 999MB |
| 7 | >= 1 gb | 1GB+ |

**io-bench currently uses 9 buckets**:
- zero, 1B-8KiB, 8KiB-64KiB, 64KiB-512KiB, 512KiB-4MiB, 4MiB-32MiB, 32MiB-256MiB, 256MiB-2GiB, >2GiB

**Decision**: Keep our 9 buckets - they provide finer granularity in the critical 1-256MB range.

### Output Format Structure

#### Per-File Output (space-aligned table)
```
       op bytes_bucket  bucket_# mean_lat_us med._lat_us 90%_lat_us 95%_lat_us 99%_lat_us  max_lat_us avg_obj_KB ops_/_sec xput_MBps    count
0  DELETE         None         0    3,516.70    3,365.36   4,496.95   4,999.16   6,425.77   25,166.27       0.00    391.14      0.00   70,405
1    STAT         None         0    2,075.24    2,003.39   2,699.74   3,022.78   3,932.32  135,365.87       0.00  1,173.38      0.00  211,210
2     GET      1 - 32k         1    2,247.77    2,149.60   2,913.59   3,255.70   4,186.27  134,553.66       4.83    218.80      1.03   39,384
3     PUT      1 - 32k         1    6,868.28    6,607.35   8,643.44   9,506.01  11,728.90   24,555.96       4.79     73.06      0.34   13,151
```

#### Consolidated Output (multi-file aggregation)
```
       op bytes_bucket  bucket_# mean_lat_us med._lat_us 90%_lat_us 95%_lat_us 99%_lat_us avg_obj_KB tot_ops_/_sec total_xput_MBps tot_count
0  DELETE         None         0    3,516.70    3,365.36   4,496.95   4,999.16   6,425.77       0.00           nan             nan   140,810
1    STAT         None         0    2,075.24    2,003.39   2,699.74   3,022.78   3,932.32       0.00           nan             nan   422,420
2     GET      1 - 32k         1    2,247.77    2,149.60   2,913.59   3,255.70   4,186.27       4.83        437.60            2.07    78,768
```

### Column Definitions

| Column | Type | Description | Notes |
|--------|------|-------------|-------|
| `op` | string | Operation type | GET, PUT, DELETE, STAT, LIST |
| `bytes_bucket` | string | Size bucket label | "1 - 32k", "32k - 128k", etc. None for metadata |
| `bucket_#` | int | Numeric bucket index | 0-7, for sorting |
| `mean_lat_us` | float | Mean latency (microseconds) | 2 decimal places, comma-separated |
| `med._lat_us` | float | Median latency (p50) | 2 decimal places |
| `90%_lat_us` | float | 90th percentile latency | 2 decimal places |
| `95%_lat_us` | float | 95th percentile latency | 2 decimal places |
| `99%_lat_us` | float | 99th percentile latency | 2 decimal places |
| `max_lat_us` | float | Maximum latency | 2 decimal places |
| `avg_obj_KB` | float | Average object size (KB) | 2 decimal places |
| `ops_/_sec` | float | Operations per second | 2 decimal places |
| `xput_MBps` | float | Throughput (MB/s) | 2 decimal places |
| `count` | int | Total operation count | Comma-separated |

### Formatting Rules
1. **Numeric formatting**: All numbers use comma separators (e.g., `3,516.70`)
2. **Float precision**: 2 decimal places for all floats
3. **Sorting**: By `bucket_#` first, then `op` alphabetically
4. **Alignment**: Space-aligned columns for human readability
5. **Headers**: Clear, concise column names with units in name

### Key Features to Adopt

1. **Dual output modes**:
   - Human-readable: Space-aligned table with comma formatting
   - Machine-readable: TSV with clean numeric values

2. **Percentile metrics**:
   - Mean, Median (p50), p90, p95, p99, Max
   - This matches HDR histogram capabilities

3. **Throughput calculations**:
   - ops/sec: `count / runtime_seconds`
   - MB/s: `(total_bytes / 1024 / 1024) / runtime_seconds`

4. **Bucket-based grouping**:
   - Group by operation AND size bucket
   - Provides granular performance insights

5. **Consolidation capability**:
   - Can aggregate multiple test runs
   - Useful for distributed testing scenarios

## Recommended TSV Format for io-bench v0.5.1

### Single File: `{basename}-results.tsv`

**Use case**: Complete results in one file for easy parsing

```tsv
operation	size_bucket	bucket_idx	mean_us	p50_us	p90_us	p95_us	p99_us	max_us	avg_bytes	ops_per_sec	throughput_mbps	count
GET	zero	0	0	0	0	0	0	0	0	0.00	0.00	0
GET	1B-8KiB	1	2247.77	2149.60	2913.59	3255.70	4186.27	134553.66	4947	218.80	1.03	39384
GET	8KiB-64KiB	2	2456.13	2364.56	3188.95	3537.03	4524.98	21507.22	102512	166.02	16.23	29883
PUT	1B-8KiB	1	6868.28	6607.35	8643.44	9506.01	11728.90	24555.96	4905	73.06	0.34	13151
DELETE	zero	0	3516.70	3365.36	4496.95	4999.16	6425.77	25166.27	0	391.14	0.00	70405
LIST	zero	0	2075.24	2003.39	2699.74	3022.78	3932.32	135365.87	0	1173.38	0.00	211210
```

**Advantages**:
- Single file = easier automation
- TSV format = easy to parse (Python, awk, Excel, etc.)
- All metrics in one place
- Column names are clear and unambiguous

### Alternative: Multiple Files (like our original plan)

If we want separation (easier to read specific metrics):

1. **`{basename}-summary.tsv`**: Overall test metadata
2. **`{basename}-operations.tsv`**: Per-operation totals (all sizes aggregated)
3. **`{basename}-buckets.tsv`**: Per-operation, per-size-bucket details (most detailed)

## Implementation Strategy for v0.5.1

### Phase 1: Core Metrics ✅ (In Progress)
- [x] Extract `OpHists` to shared `metrics` module
- [ ] Update `workload.rs` to use size-bucketed histograms
- [ ] Ensure all operations record with `bucket_index(bytes.len())`

### Phase 2: TSV Export
- [ ] Create `tsv_export.rs` module
- [ ] Implement single-file TSV format (recommended)
- [ ] Add `--results-tsv <basename>` CLI flag
- [ ] Format numbers with precision (no comma separators in TSV - that's for display)

### Phase 3: Display Formatting
- [ ] Keep existing console output (human-readable)
- [ ] Optionally add polarWarp-style table formatting
- [ ] Comma-separated numbers for readability

### Phase 4: Testing
- [ ] Run workload with TSV export
- [ ] Verify TSV parsing with Python/pandas
- [ ] Compare metrics with manual calculation
- [ ] Test with different workload configs

## Code Snippets from polarWarp

### Bucket Definition (Python/Polars)
```python
bucket_order = ["NaN", "1 - 32k", "32k - 128k", "128k - 1mb", "1m - 8mb", "8m - 64mb", "64m - 999mb", ">= 1 gb"]

df = df.with_columns([
    pl.when(pl.col("bytes") == 0).then(pl.lit(None))
    .when((pl.col("bytes") >= 1) & (pl.col("bytes") < 32768)).then(pl.lit("1 - 32k"))
    .when((pl.col("bytes") >= 32768) & (pl.col("bytes") < 131072)).then(pl.lit("32k - 128k"))
    # ... more buckets
])
```

### Aggregation (Python/Polars)
```python
result = df.group_by(["op", "bytes_bucket", "bucket_#"]).agg([
    (pl.col("duration_ns").mean() / 1000).alias("mean_lat_us"),
    (pl.col("duration_ns").median() / 1000).alias("med._lat_us"),
    (pl.col("duration_ns").quantile(0.90) / 1000).alias("90%_lat_us"),
    (pl.col("duration_ns").quantile(0.95) / 1000).alias("95%_lat_us"),
    (pl.col("duration_ns").quantile(0.99) / 1000).alias("99%_lat_us"),
    (pl.col("duration_ns").max() / 1000).alias("max_lat_us"),
    (pl.col("bytes").mean() / 1024).alias("avg_obj_KB"),
    (pl.count("op") / run_time_secs).alias("ops_/_sec"),
    ((pl.col("bytes").sum() / (1024 * 1024)) / run_time_secs).alias("xput_MBps"),
    pl.count("op").alias("count")
])
```

### Rust Equivalent (for io-bench)
```rust
// In tsv_export.rs
fn write_bucket_row(&self, f: &mut File, op: &str, bucket_idx: usize, bucket_label: &str, hist: &Histogram<u64>, stats: &BucketStats) -> Result<()> {
    writeln!(f, "{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.2}\t{:.2}\t{}",
        op,                          // operation
        bucket_label,                // size_bucket
        bucket_idx,                  // bucket_idx
        hist.mean(),                 // mean_us
        hist.value_at_quantile(0.50) as f64,  // p50_us
        hist.value_at_quantile(0.90) as f64,  // p90_us
        hist.value_at_quantile(0.95) as f64,  // p95_us
        hist.value_at_quantile(0.99) as f64,  // p99_us
        hist.max() as f64,           // max_us
        stats.avg_bytes,             // avg_bytes
        stats.ops_per_sec,           // ops_per_sec
        stats.throughput_mbps,       // throughput_mbps
        stats.count,                 // count
    )?;
    Ok(())
}
```

## Conclusion

polarWarp provides an excellent reference for:
1. **Output format**: Clean, parseable, comprehensive
2. **Size bucketing**: Demonstrates value of bucketed metrics
3. **Percentile reporting**: p50, p90, p95, p99, max
4. **Throughput calculations**: ops/sec and MB/s
5. **Display formatting**: Comma-separated numbers for humans

io-bench v0.5.1 should adopt similar principles while maintaining:
- Our 9-bucket system (finer granularity)
- Our HDR histogram accuracy
- Our multi-backend support
- Rust performance advantages
