//! Results directory management for sai3-bench
//!
//! Automatically creates structured output directories containing:
//! - TSV metrics results
//! - Console output logs
//! - Configuration file copy
//! - Run metadata (JSON)
//!
//! Directory format: sai3-{YYYYMMDD}-{HHMM}-{test_name}/

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Metadata about a benchmark run
#[derive(Debug, Serialize, Deserialize)]
pub struct RunMetadata {
    pub version: String,
    pub test_name: String,
    pub config_path: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub duration_secs: Option<f64>,
    pub command_line: Vec<String>,
    pub hostname: String,
    pub distributed: bool,
    pub agents: Option<Vec<String>>,
}

impl RunMetadata {
    pub fn new(test_name: String, config_path: String) -> Self {
        let version = env!("CARGO_PKG_VERSION").to_string();
        let start_time = Local::now().to_rfc3339();
        let hostname = hostname::get()
            .unwrap_or_else(|_| "unknown".into())
            .to_string_lossy()
            .to_string();
        let command_line = std::env::args().collect();

        Self {
            version,
            test_name,
            config_path,
            start_time,
            end_time: None,
            duration_secs: None,
            command_line,
            hostname,
            distributed: false,
            agents: None,
        }
    }

    pub fn finalize(&mut self, duration_secs: f64) {
        self.end_time = Some(Local::now().to_rfc3339());
        self.duration_secs = Some(duration_secs);
    }
}

/// Results directory manager
pub struct ResultsDir {
    path: PathBuf,
    metadata: RunMetadata,
    console_log: Option<fs::File>,
}

impl ResultsDir {
    /// Create a new results directory with the standard naming convention
    /// 
    /// # Arguments
    /// * `config_path` - Path to the config file (used for default test name)
    /// * `custom_name` - Optional custom name to use instead of config filename
    /// * `base_dir` - Optional base directory (defaults to current directory)
    pub fn create(config_path: &Path, custom_name: Option<&str>, base_dir: Option<&Path>) -> Result<Self> {
        // Extract test name from config filename or use custom name
        let test_name = if let Some(name) = custom_name {
            name.to_string()
        } else {
            config_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test")
                .to_string()
        };

        // Generate directory name: sai3-{YYYYMMDD}-{HHMM}-{test_name}
        let now = Local::now();
        let dir_name = format!(
            "sai3-{}-{}",
            now.format("%Y%m%d-%H%M"),
            test_name
        );

        // Determine base directory
        let base = base_dir.unwrap_or_else(|| Path::new("."));
        let dir_path = base.join(&dir_name);

        // Create directory
        fs::create_dir_all(&dir_path)
            .with_context(|| format!("Failed to create results directory: {}", dir_path.display()))?;

        // Copy config file
        let config_dest = dir_path.join("config.yaml");
        fs::copy(config_path, &config_dest)
            .with_context(|| "Failed to copy config to results directory".to_string())?;

        // Create metadata
        let metadata = RunMetadata::new(
            test_name,
            config_path.to_string_lossy().to_string(),
        );

        // Create console log file
        let console_log_path = dir_path.join("console.log");
        let console_log = fs::File::create(&console_log_path)
            .with_context(|| "Failed to create console.log".to_string())?;

        tracing::info!("Created results directory: {}", dir_path.display());

        Ok(Self {
            path: dir_path,
            metadata,
            console_log: Some(console_log),
        })
    }

    /// Get the path to the results directory
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the path for the TSV results file
    pub fn tsv_path(&self) -> PathBuf {
        self.path.join("workload_results.tsv")
    }

    /// Get the path for the prepare phase TSV results file
    pub fn prepare_tsv_path(&self) -> PathBuf {
        self.path.join("prepare_results.tsv")
    }

    /// Get the path for console log
    pub fn console_log_path(&self) -> PathBuf {
        self.path.join("console.log")
    }

    /// Write a line to the console log
    pub fn write_console(&mut self, line: &str) -> Result<()> {
        if let Some(ref mut log) = self.console_log {
            writeln!(log, "{}", line)
                .with_context(|| "Failed to write to console.log")?;
        }
        Ok(())
    }

    /// Write metadata to metadata.json
    pub fn write_metadata(&self) -> Result<()> {
        let metadata_path = self.path.join("metadata.json");
        let json = serde_json::to_string_pretty(&self.metadata)
            .with_context(|| "Failed to serialize metadata")?;
        fs::write(&metadata_path, json)
            .with_context(|| "Failed to write metadata.json".to_string())?;
        Ok(())
    }

    /// Finalize the results directory (write final metadata)
    pub fn finalize(&mut self, duration_secs: f64) -> Result<()> {
        self.metadata.finalize(duration_secs);
        self.write_metadata()?;
        
        // Flush and close console log
        if let Some(mut log) = self.console_log.take() {
            log.flush()?;
        }

        tracing::info!("Results saved to: {}", self.path.display());
        Ok(())
    }

    /// Create agents subdirectory for distributed runs
    pub fn create_agents_dir(&mut self) -> Result<PathBuf> {
        self.metadata.distributed = true;
        let agents_dir = self.path.join("agents");
        fs::create_dir_all(&agents_dir)
            .with_context(|| "Failed to create agents subdirectory")?;
        Ok(agents_dir)
    }

    /// Add an agent to the metadata
    pub fn add_agent(&mut self, agent_name: String) {
        if self.metadata.agents.is_none() {
            self.metadata.agents = Some(Vec::new());
        }
        if let Some(ref mut agents) = self.metadata.agents {
            agents.push(agent_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_results_dir_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.yaml");
        fs::write(&config_path, "# test config").unwrap();

        let results_dir = ResultsDir::create(&config_path, None, Some(temp_dir.path())).unwrap();

        assert!(results_dir.path().exists());
        assert!(results_dir.path().join("config.yaml").exists());
        assert!(results_dir.path().join("console.log").exists());
    }

    #[test]
    fn test_metadata_serialization() {
        let metadata = RunMetadata::new("test".to_string(), "config.yaml".to_string());
        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("\"version\""));
        assert!(json.contains("\"test_name\":\"test\""));
    }
}
