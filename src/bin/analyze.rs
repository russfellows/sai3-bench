//! sai3bench-analyze - Results consolidation tool
//!
//! Consolidates multiple sai3-bench results directories into a single Excel spreadsheet.
//! Each results directory contributes two tabs: one for results.tsv and one for prepare_results.tsv.
//!
//! # Usage
//!
//! ```bash
//! # Analyze all results in current directory
//! sai3bench-analyze --pattern "sai3-*" --output consolidated.xlsx
//!
//! # Analyze specific directories
//! sai3bench-analyze --dirs dir1,dir2,dir3 --output results.xlsx
//!
//! # Include per-agent results (future)
//! # sai3bench-analyze --pattern "sai3-*" --include-agents --output full.xlsx
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use rust_xlsxwriter::{Format, Workbook, Worksheet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "sai3bench-analyze")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Consolidate sai3-bench results into Excel spreadsheet", long_about = None)]
struct Args {
    /// Glob pattern for results directories (e.g., "sai3-*")
    #[arg(short, long)]
    pattern: Option<String>,

    /// Comma-separated list of specific directories to analyze
    #[arg(short, long, value_delimiter = ',')]
    dirs: Vec<PathBuf>,

    /// Base directory to search for results (default: current directory)
    #[arg(short, long, default_value = ".")]
    base_dir: PathBuf,

    /// Output Excel file path
    #[arg(short, long, default_value = "sai3bench-results.xlsx")]
    output: PathBuf,

    /// Include per-agent results (future feature)
    #[arg(long)]
    include_agents: bool,
}

/// Information extracted from a results directory
#[derive(Debug)]
struct ResultsDir {
    path: PathBuf,
    timestamp: String,
    workload: String,
    hosts: String,
    results_tsv: Option<PathBuf>,
    prepare_results_tsv: Option<PathBuf>,
    perf_log_tsv: Option<PathBuf>,
    workload_endpoint_stats_tsv: Option<PathBuf>,
    prepare_endpoint_stats_tsv: Option<PathBuf>,
}

impl ResultsDir {
    /// Parse a results directory name to extract metadata
    /// Expected format: sai3-YYYYMMDD-HHMM-workload_Xhosts
    fn from_path(path: PathBuf) -> Result<Self> {
        let dir_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid directory name")?;

        // Parse directory name: sai3-20251222-1744-sai3-resnet50_1hosts
        // First split by underscore to separate workload from host count
        let (timestamp, workload, hosts) = if let Some(underscore_pos) = dir_name.rfind('_') {
            let before_underscore = &dir_name[..underscore_pos];
            let after_underscore = &dir_name[underscore_pos + 1..];
            
            // Split the part before underscore by dashes
            let parts: Vec<&str> = before_underscore.split('-').collect();
            
            if parts.len() >= 4 {
                // Extract date and time
                let date = parts.get(1).unwrap_or(&"unknown");
                let time = parts.get(2).unwrap_or(&"0000");
                let timestamp = format!("{}-{}", date, time);
                
                // Everything from index 3 onwards is the workload name
                let workload = parts[3..].join("-");
                
                // Extract host count (e.g., "1hosts", "8hosts")
                let hosts = after_underscore.to_string();
                
                (timestamp, workload, hosts)
            } else {
                // Fallback for non-standard directory names
                (dir_name.to_string(), "unknown".to_string(), "?".to_string())
            }
        } else {
            // No underscore found, fallback
            (dir_name.to_string(), "unknown".to_string(), "?".to_string())
        };

        // Check for TSV files
        let results_tsv = path.join("results.tsv");
        let prepare_results_tsv = path.join("prepare_results.tsv");
        let perf_log_tsv = path.join("perf_log.tsv");
        let workload_endpoint_stats_tsv = path.join("workload_endpoint_stats.tsv");
        let prepare_endpoint_stats_tsv = path.join("prepare_endpoint_stats.tsv");

        Ok(Self {
            path,
            timestamp,
            workload,
            hosts,
            results_tsv: if results_tsv.exists() { Some(results_tsv) } else { None },
            prepare_results_tsv: if prepare_results_tsv.exists() { 
                Some(prepare_results_tsv) 
            } else { 
                None 
            },
            perf_log_tsv: if perf_log_tsv.exists() {
                Some(perf_log_tsv)
            } else {
                None
            },
            workload_endpoint_stats_tsv: if workload_endpoint_stats_tsv.exists() {
                Some(workload_endpoint_stats_tsv)
            } else {
                None
            },
            prepare_endpoint_stats_tsv: if prepare_endpoint_stats_tsv.exists() {
                Some(prepare_endpoint_stats_tsv)
            } else {
                None
            },
        })
    }

    /// Generate a short, unique tab name (Excel limit: 31 chars)
    /// Format: MMDD-HHMM-workload_Xh-R/P/L/WE/PE
    fn generate_tab_name(&self, file_type: &str) -> String {
        // Extract MMDD-HHMM from timestamp (20251222-1744 -> 1222-1744)
        let short_time = if self.timestamp.len() >= 13 {
            format!("{}-{}", &self.timestamp[4..8], &self.timestamp[9..13])
        } else {
            self.timestamp.clone()
        };

        // Shorten workload name (remove "sai3-" prefix if present)
        let short_workload = self.workload
            .strip_prefix("sai3-")
            .unwrap_or(&self.workload);

        // Shorten host count (1hosts -> 1h, 8hosts -> 8h)
        let short_hosts = self.hosts.replace("hosts", "h");

        // Combine: 1222-1744-resnet50_1h-R
        let mut tab_name = format!("{}-{}_{}-{}", 
            short_time, 
            short_workload, 
            short_hosts,
            file_type
        );

        // Truncate to 31 characters if needed (preserve file_type suffix)
        if tab_name.len() > 31 {
            // Find the workload portion and trim it
            let suffix = format!("_{}-{}", short_hosts, file_type);
            let prefix_len = short_time.len() + 1; // "MMDD-HHMM-"
            let workload_max_len = 31 - prefix_len - suffix.len();
            
            let trimmed_workload = if short_workload.len() > workload_max_len {
                &short_workload[..workload_max_len]
            } else {
                short_workload
            };
            
            tab_name = format!("{}-{}{}", short_time, trimmed_workload, suffix);
        }

        tab_name
    }
}

/// Find all results directories matching the pattern or list
fn find_results_dirs(args: &Args) -> Result<Vec<ResultsDir>> {
    let mut results_dirs = Vec::new();

    // If specific directories provided, use those
    if !args.dirs.is_empty() {
        for dir in &args.dirs {
            let full_path = args.base_dir.join(dir);
            if full_path.exists() && full_path.is_dir() {
                if let Ok(results_dir) = ResultsDir::from_path(full_path) {
                    results_dirs.push(results_dir);
                }
            } else {
                eprintln!("Warning: Directory not found or not a directory: {:?}", full_path);
            }
        }
        return Ok(results_dirs);
    }

    // Otherwise, use pattern matching
    let pattern = args.pattern.as_deref().unwrap_or("sai3-*");
    
    // Walk the base directory looking for matching directories
    for entry in WalkDir::new(&args.base_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };

        // Simple pattern matching (could be enhanced with glob crate)
        if pattern.contains('*') {
            let prefix = pattern.trim_end_matches('*');
            if !dir_name.starts_with(prefix) {
                continue;
            }
        } else if dir_name != pattern {
            continue;
        }

        // Skip the base directory itself
        if path == args.base_dir {
            continue;
        }

        if let Ok(results_dir) = ResultsDir::from_path(path.to_path_buf()) {
            // Only include if it has at least one TSV file
            if results_dir.results_tsv.is_some() 
                || results_dir.prepare_results_tsv.is_some() 
                || results_dir.perf_log_tsv.is_some() 
            {
                results_dirs.push(results_dir);
            }
        }
    }

    // Sort by timestamp for consistent ordering
    results_dirs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    Ok(results_dirs)
}

/// Read a TSV file and return its contents as rows
fn read_tsv_file(path: &Path) -> Result<Vec<Vec<String>>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read TSV file: {:?}", path))?;

    let rows: Vec<Vec<String>> = content
        .lines()
        .map(|line| {
            line.split('\t')
                .map(|cell| cell.to_string())
                .collect()
        })
        .collect();

    Ok(rows)
}

/// Write TSV data to an Excel worksheet
fn write_tsv_to_worksheet(
    worksheet: &mut Worksheet,
    rows: &[Vec<String>],
    header_format: &Format,
    data_format: &Format,
) -> Result<()> {
    // Create a datetime format for timestamp columns
    let datetime_format = Format::new()
        .set_font_name("Aptos")
        .set_num_format("yyyy-mm-dd hh:mm:ss.000");
    
    // Detect timestamp columns from header (row 0)
    // Returns (column_index, divisor_for_seconds)
    let timestamp_cols: Vec<(usize, f64)> = if let Some(header_row) = rows.first() {
        header_row.iter()
            .enumerate()
            .filter_map(|(idx, col_name)| {
                if col_name.contains("timestamp_epoch_ms") || col_name.contains("time_ms") {
                    Some((idx, 1000.0))  // Milliseconds
                } else if col_name.contains("timestamp_epoch_us") || col_name.contains("time_us") {
                    Some((idx, 1_000_000.0))  // Microseconds
                } else if col_name.contains("timestamp_epoch_ns") || col_name.contains("time_ns") {
                    Some((idx, 1_000_000_000.0))  // Nanoseconds
                } else if col_name.contains("timestamp_epoch") || col_name.contains("timestamp") {
                    Some((idx, 1.0))  // Assume seconds if no unit specified
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    
    for (row_idx, row) in rows.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            let row = row_idx as u32;
            let col = col_idx as u16;

            if row_idx == 0 {
                // Header row - use bold format
                worksheet.write_string_with_format(row, col, cell, header_format)?;
            } else if let Some(&(_, divisor)) = timestamp_cols.iter().find(|(idx, _)| *idx == col_idx) {
                // Timestamp column - convert Unix epoch to Excel datetime
                if let Ok(epoch_value) = cell.parse::<i64>() {
                    // Convert to seconds based on the unit (ms, us, ns, or s)
                    let epoch_seconds = epoch_value as f64 / divisor;
                    // Convert to Excel datetime:
                    // 1. Convert seconds to days: seconds / 86400
                    // 2. Add Excel epoch offset: 25569 days (Jan 1, 1970 - Jan 1, 1900)
                    let excel_datetime = (epoch_seconds / 86400.0) + 25569.0;
                    worksheet.write_number_with_format(row, col, excel_datetime, &datetime_format)?;
                } else {
                    // If parsing fails, write as string
                    worksheet.write_string_with_format(row, col, cell, data_format)?;
                }
            } else {
                // Regular data cell - try to parse as number first
                if let Ok(num) = cell.parse::<f64>() {
                    worksheet.write_number_with_format(row, col, num, data_format)?;
                } else {
                    // Write as string
                    worksheet.write_string_with_format(row, col, cell, data_format)?;
                }
            }
        }
    }

    // Auto-fit columns (approximate based on content)
    if let Some(first_row) = rows.first() {
        for (col_idx, _header) in first_row.iter().enumerate() {
            // Set wider width for timestamp columns
            if timestamp_cols.iter().any(|(idx, _)| *idx == col_idx) {
                worksheet.set_column_width(col_idx as u16, 22)?;
            } else {
                // Set a reasonable default width
                worksheet.set_column_width(col_idx as u16, 15)?;
            }
        }
    }

    Ok(())
}

/// Create Excel workbook with consolidated results
fn create_excel_workbook(results_dirs: &[ResultsDir], output_path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();

    // Create a bold format for headers with Aptos font
    let header_format = Format::new()
        .set_bold()
        .set_font_name("Aptos");
    
    // Create a regular format with Aptos font for data cells
    let data_format = Format::new()
        .set_font_name("Aptos");

    let mut tabs_created = 0;

    for results_dir in results_dirs {
        println!("Processing: {:?}", results_dir.path);

        // Add results.tsv tab
        if let Some(ref tsv_path) = results_dir.results_tsv {
            let tab_name = results_dir.generate_tab_name("R");
            println!("  Creating tab: {} (results.tsv)", tab_name);

            let rows = read_tsv_file(tsv_path)
                .with_context(|| format!("Failed to read results.tsv from {:?}", results_dir.path))?;

            let worksheet = workbook.add_worksheet();
            worksheet.set_name(&tab_name)?;
            write_tsv_to_worksheet(worksheet, &rows, &header_format, &data_format)?;

            tabs_created += 1;
        }

        // Add prepare_results.tsv tab
        if let Some(ref tsv_path) = results_dir.prepare_results_tsv {
            let tab_name = results_dir.generate_tab_name("P");
            println!("  Creating tab: {} (prepare_results.tsv)", tab_name);

            let rows = read_tsv_file(tsv_path)
                .with_context(|| format!("Failed to read prepare_results.tsv from {:?}", results_dir.path))?;

            let worksheet = workbook.add_worksheet();
            worksheet.set_name(&tab_name)?;
            write_tsv_to_worksheet(worksheet, &rows, &header_format, &data_format)?;

            tabs_created += 1;
        }

        // Add perf_log.tsv tab
        if let Some(ref tsv_path) = results_dir.perf_log_tsv {
            let tab_name = results_dir.generate_tab_name("L");
            println!("  Creating tab: {} (perf_log.tsv)", tab_name);

            let rows = read_tsv_file(tsv_path)
                .with_context(|| format!("Failed to read perf_log.tsv from {:?}", results_dir.path))?;

            let worksheet = workbook.add_worksheet();
            worksheet.set_name(&tab_name)?;
            write_tsv_to_worksheet(worksheet, &rows, &header_format, &data_format)?;

            tabs_created += 1;
        }

        // Add workload_endpoint_stats.tsv tab
        if let Some(ref tsv_path) = results_dir.workload_endpoint_stats_tsv {
            let tab_name = results_dir.generate_tab_name("WE");
            println!("  Creating tab: {} (workload_endpoint_stats.tsv)", tab_name);

            let rows = read_tsv_file(tsv_path)
                .with_context(|| format!("Failed to read workload_endpoint_stats.tsv from {:?}", results_dir.path))?;

            let worksheet = workbook.add_worksheet();
            worksheet.set_name(&tab_name)?;
            write_tsv_to_worksheet(worksheet, &rows, &header_format, &data_format)?;

            tabs_created += 1;
        }

        // Add prepare_endpoint_stats.tsv tab
        if let Some(ref tsv_path) = results_dir.prepare_endpoint_stats_tsv {
            let tab_name = results_dir.generate_tab_name("PE");
            println!("  Creating tab: {} (prepare_endpoint_stats.tsv)", tab_name);

            let rows = read_tsv_file(tsv_path)
                .with_context(|| format!("Failed to read prepare_endpoint_stats.tsv from {:?}", results_dir.path))?;

            let worksheet = workbook.add_worksheet();
            worksheet.set_name(&tab_name)?;
            write_tsv_to_worksheet(worksheet, &rows, &header_format, &data_format)?;

            tabs_created += 1;
        }
    }

    if tabs_created == 0 {
        anyhow::bail!("No TSV files found in any results directory");
    }

    // Save the workbook
    workbook.save(output_path)
        .with_context(|| format!("Failed to save Excel file: {:?}", output_path))?;

    println!("\nSuccess! Created {} tabs in {:?}", tabs_created, output_path);

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("sai3bench-analyze - Results Consolidation Tool");
    println!("============================================\n");

    if args.include_agents {
        eprintln!("Warning: --include-agents is not yet implemented");
    }

    // Find all results directories
    println!("Searching for results directories...");
    let results_dirs = find_results_dirs(&args)?;

    if results_dirs.is_empty() {
        anyhow::bail!(
            "No results directories found matching pattern in {:?}\n\
             Use --pattern 'sai3-*' or --dirs dir1,dir2,dir3",
            args.base_dir
        );
    }

    println!("Found {} results directories:\n", results_dirs.len());
    for (idx, dir) in results_dirs.iter().enumerate() {
        println!("  {}. {:?}", idx + 1, dir.path.file_name().unwrap_or_default());
        if dir.results_tsv.is_some() {
            println!("      - results.tsv found");
        }
        if dir.prepare_results_tsv.is_some() {
            println!("      - prepare_results.tsv found");
        }
        if dir.perf_log_tsv.is_some() {
            println!("      - perf_log.tsv found");
        }
    }
    println!();

    // Create Excel workbook
    println!("Creating Excel workbook...\n");
    create_excel_workbook(&results_dirs, &args.output)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_standard_directory_name() {
        let path = PathBuf::from("sai3-20251222-1744-sai3-resnet50_1hosts");
        let results_dir = ResultsDir::from_path(path).unwrap();
        
        assert_eq!(results_dir.timestamp, "20251222-1744");
        assert_eq!(results_dir.workload, "sai3-resnet50");
        assert_eq!(results_dir.hosts, "1hosts");
    }

    #[test]
    fn test_generate_tab_name() {
        let results_dir = ResultsDir {
            path: PathBuf::from("sai3-20251222-1744-sai3-resnet50_1hosts"),
            timestamp: "20251222-1744".to_string(),
            workload: "sai3-resnet50".to_string(),
            hosts: "1hosts".to_string(),
            results_tsv: None,
            prepare_results_tsv: None,
            perf_log_tsv: None,
            workload_endpoint_stats_tsv: None,
            prepare_endpoint_stats_tsv: None,
        };

        let tab_name = results_dir.generate_tab_name("R");
        assert_eq!(tab_name, "1222-1744-resnet50_1h-R");
        assert!(tab_name.len() <= 31, "Tab name exceeds Excel limit");
    }

    #[test]
    fn test_tab_name_truncation() {
        let results_dir = ResultsDir {
            path: PathBuf::from("test"),
            timestamp: "20251222-1744".to_string(),
            workload: "very-long-workload-name-that-exceeds-limits".to_string(),
            hosts: "8hosts".to_string(),
            results_tsv: None,
            prepare_results_tsv: None,
            perf_log_tsv: None,
            workload_endpoint_stats_tsv: None,
            prepare_endpoint_stats_tsv: None,
        };

        let tab_name = results_dir.generate_tab_name("R");
        assert!(tab_name.len() <= 31, "Tab name not truncated properly");
    }
}
