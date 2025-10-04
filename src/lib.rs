// src/lib.rs

use regex::escape;

pub mod config;
pub mod metrics; // Shared metrics infrastructure (v0.5.1+)
pub mod replay; // Legacy in-memory replay (v0.4.0)
pub mod replay_streaming; // New streaming replay (v0.5.0+)
pub mod remap; // Advanced remapping for replay (v0.5.0+)
pub mod tsv_export; // TSV export for machine-readable results (v0.5.1+)
pub mod workload;

// Re-export bucket_index from metrics for backward compatibility
pub use metrics::bucket_index;

/// Converts a simple glob (with `*`) into a fully-anchored regex string.
pub fn glob_to_regex(glob: &str) -> String {
    format!("^{}$", escape(glob).replace(r"\*", ".*"))
}

