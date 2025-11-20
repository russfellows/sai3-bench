// src/lib.rs

use regex::escape;

pub mod config;
pub mod constants; // Central constants and defaults (v0.8.0+)
pub mod cpu_monitor;
pub mod directory_tree; // Directory structure generation (width/depth model)
pub mod live_stats; // Live stats tracking for distributed execution (v0.7.5+)
pub mod metadata_prefetch; // Async metadata pre-fetching (v0.6.9+)
pub mod metrics; // Shared metrics infrastructure (v0.5.1+)
pub mod multiprocess; // Multi-process scaling for parallel I/O (v0.7.3+)
pub mod multiruntime; // Multi-runtime scaling with separate tokio runtimes (v0.7.3+)
pub mod oplog_merge; // Op-log merging for multi-worker execution (v0.7.3+)
pub mod prepare; // Prepare phase: object pre-population and directory trees (v0.7.2+)
pub mod rate_controller; // I/O rate control (v0.7.1+)
pub mod replay; // Legacy in-memory replay (v0.4.0)
pub mod replay_streaming; // New streaming replay (v0.5.0+)
pub mod remap; // Advanced remapping for replay (v0.5.0+)
pub mod results_dir; // Results directory management (v0.6.4+)
pub mod size_generator; // Size distributions for realistic workloads (v0.5.3+)
pub mod ssh_deploy; // SSH-based distributed deployment (v0.6.11+)
pub mod ssh_setup; // SSH key setup automation (v0.6.11+)
pub mod tsv_export; // TSV export for machine-readable results (v0.5.1+)
pub mod validation; // Configuration validation and summary display (v0.7.12+)
pub mod workload;

// Re-export bucket_index from metrics for backward compatibility
pub use metrics::bucket_index;

/// Converts a simple glob (with `*`) into a fully-anchored regex string.
pub fn glob_to_regex(glob: &str) -> String {
    format!("^{}$", escape(glob).replace(r"\*", ".*"))
}

