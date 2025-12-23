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
        })
    }

    /// Generate a short, unique tab name (Excel limit: 31 chars)
    /// Format: MMDD-HHMM-workload_Xh-R/P
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
        let tab_name = format!("{}-{}_{}-{}", 
            short_time, 
            short_workload, 
            short_hosts,
            file_type
        );

        // Truncate to 31 characters if needed
        if tab_name.len() > 31 {
            tab_name[..31].to_string()
        } else {
            tab_name
        }
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
            if results_dir.results_tsv.is_some() || results_dir.prepare_results_tsv.is_some() {
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
) -> Result<()> {
    for (row_idx, row) in rows.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            let row = row_idx as u32;
            let col = col_idx as u16;

            // Try to parse as number first
            if let Ok(num) = cell.parse::<f64>() {
                worksheet.write_number(row, col, num)?;
            } else {
                // Write as string
                if row_idx == 0 {
                    // Header row - use bold format
                    worksheet.write_string_with_format(row, col, cell, header_format)?;
                } else {
                    worksheet.write_string(row, col, cell)?;
                }
            }
        }
    }

    // Auto-fit columns (approximate based on content)
    if let Some(first_row) = rows.first() {
        for (col_idx, _) in first_row.iter().enumerate() {
            // Set a reasonable default width
            worksheet.set_column_width(col_idx as u16, 15)?;
        }
    }

    Ok(())
}

/// Create Excel workbook with consolidated results
fn create_excel_workbook(results_dirs: &[ResultsDir], output_path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();

    // Create a bold format for headers
    let header_format = Format::new().set_bold();

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
            write_tsv_to_worksheet(worksheet, &rows, &header_format)?;

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
            write_tsv_to_worksheet(worksheet, &rows, &header_format)?;

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
        };

        let tab_name = results_dir.generate_tab_name("R");
        assert!(tab_name.len() <= 31, "Tab name not truncated properly");
    }
}
